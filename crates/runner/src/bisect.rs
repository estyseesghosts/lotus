//! First-bad search over a bounded first-parent range with locally approved build profiles.
use crate::{
    assessment::Status,
    execution,
    history::{self, HISTORY_DIRECTORY},
    suite::{self, bounded_file, identifier, PreparedSuite},
};
use lotus_protocol::envelope::SessionUuid;
use lotus_session::supervisor::{regression_workspace_bytes, Supervisor};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;

const MAX_REVISIONS: usize = 8;
const WALL_SECONDS: u64 = 900;
const MAX_EMULATION_ATTEMPTS: usize = 64;
const MAX_REPORT_BYTES: u64 = 64 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    version: u32,
    git: PathBuf,
    repository: PathBuf,
    suite: PathBuf,
    good: String,
    bad: String,
    profiles: BTreeMap<String, String>,
}

#[derive(Serialize)]
pub struct Trial {
    pub revision: String,
    pub profile: String,
    pub status: Status,
    pub fixture_sha256: Option<String>,
    pub suite: Option<String>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct Report {
    pub version: u32,
    pub id: String,
    pub good: String,
    pub bad: String,
    pub first_bad: Option<String>,
    pub worker_sha256: String,
    pub reason: Option<&'static str>,
    pub trials: Vec<Trial>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

async fn git(program: &Path, root: &Path, args: &[&str]) -> io::Result<String> {
    let output = tokio::time::timeout(Duration::from_secs(5), async {
        let mut child = tokio::process::Command::new(program)
            .args(["--no-pager", "--no-optional-locks", "--no-replace-objects"])
            .args(["-c", "core.fsmonitor=false"])
            .args(args)
            .env_clear()
            .envs(
                ["SystemRoot", "PATH", "TEMP", "TMP"]
                    .iter()
                    .filter_map(|key| Some((key, std::env::var_os(key)?))),
            )
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .kill_on_drop(true)
            .spawn()?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .expect("piped Git stdout")
            .take(32769)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > 32768 || !child.wait().await?.success() {
            return Err(invalid(format!(
                "git {} failed or exceeded its output limit",
                args[0]
            )));
        }
        String::from_utf8(bytes).map_err(io::Error::other)
    })
    .await;
    output.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "git inspection timed out"))?
}

async fn common_dir(program: &Path, root: &Path) -> io::Result<PathBuf> {
    let result = git(
        program,
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    Path::new(result.trim()).canonicalize()
}

async fn check_worktree(
    program: &Path,
    root: &Path,
    common: &Path,
    revision: &str,
) -> io::Result<()> {
    // Separate linked worktrees leave the user's primary checkout untouched.
    if !root.join(".git").is_file() || common_dir(program, root).await? != common {
        return Err(invalid(
            "build profile root must be an approved linked worktree of this repository",
        ));
    }
    let head = git(program, root, &["rev-parse", "--verify", "HEAD"]).await?;
    if head.trim() != revision
        || !git(
            program,
            root,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )
        .await?
        .is_empty()
    {
        return Err(invalid(
            "build profile worktree must be clean at its assigned revision",
        ));
    }
    Ok(())
}

fn estimate(suite: &PreparedSuite, revisions: usize) -> io::Result<u64> {
    let mut bytes = 0;
    for (_, case) in &suite.cases {
        let (_, scenario) = case.as_ref().map_err(|error| invalid(error.to_string()))?;
        if scenario.content.sha256.is_some() || scenario.movie.is_some() {
            return Err(invalid(
                "bisect scenarios use the built fixture and cannot pin content or use movies",
            ));
        }
        // A capture saves both 400x240 and 320x240 BGRA screens. The runner also
        // captures the final frame. Leave room for the small retained reports.
        bytes += (scenario.captures.len() as u64 + 1) * 691_200 + (1 << 20);
    }
    Ok(bytes * u64::from(suite.repeats) * revisions as u64)
}

struct Search<'a> {
    host: &'a Supervisor,
    plan: Plan,
    suite: PreparedSuite,
    common: PathBuf,
    revisions: Vec<String>,
    output: PathBuf,
    worker_hash: String,
    deadline: tokio::time::Instant,
    quota: u64,
    trials: BTreeMap<usize, Trial>,
}

impl Search<'_> {
    async fn execute(
        &self,
        revision: &str,
        profile: &str,
        trial: &mut Trial,
    ) -> io::Result<Status> {
        let root = &self.host.config().build_profiles[profile].root;
        check_worktree(&self.plan.git, root, &self.common, revision).await?;
        let fixture = self.host.build_regression_fixture(profile).await?;
        trial.fixture_sha256 = Some(fixture.sha256.clone());
        check_worktree(&self.plan.git, root, &self.common, revision).await?;
        let worker = &self.host.config().worker;
        let report = tokio::time::timeout_at(
            self.deadline,
            suite::run_prepared(
                &self.suite,
                worker,
                &self.output,
                revision,
                Some((&fixture.path, &fixture.sha256)),
            ),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "bisect emulation deadline"))??;
        trial.suite = Some(report.id.clone());
        regression_workspace_bytes(&self.output, self.quota)?;
        if report.worker_sha256 != self.worker_hash {
            return Err(invalid("worker identity changed between revisions"));
        }
        if matches!(report.status, Status::Pass | Status::Fail) {
            let retained = history::detail(&self.output, &report.id)?;
            if retained.cases.iter().any(|case| {
                !matches!(case.status, Status::Pass | Status::Fail)
                    || case.content_sha256.as_deref() != Some(&fixture.sha256)
            }) {
                return Err(invalid(
                    "retained repeats could not be verified against the built fixture",
                ));
            }
        }
        Ok(report.status)
    }

    async fn evaluate(&mut self, index: usize) -> Status {
        if let Some(trial) = self.trials.get(&index) {
            return trial.status;
        }
        let revision = &self.revisions[index];
        let profile = &self.plan.profiles[revision];
        let mut trial = Trial {
            revision: revision.clone(),
            profile: profile.clone(),
            status: Status::Incomplete,
            fixture_sha256: None,
            suite: None,
            error: None,
        };
        match tokio::time::timeout_at(self.deadline, self.execute(revision, profile, &mut trial))
            .await
        {
            Ok(Ok(status)) => trial.status = status,
            Ok(Err(error)) => trial.error = Some(error.to_string().chars().take(512).collect()),
            Err(_) => trial.error = Some("bisect wall deadline".into()),
        };
        let status = trial.status;
        self.trials.insert(index, trial);
        status
    }

    async fn first_bad(&mut self) -> Result<String, &'static str> {
        if self.evaluate(0).await != Status::Pass {
            return Err("the good endpoint did not produce a stable PASS");
        }
        let mut low = 0;
        let mut high = self.revisions.len() - 1;
        if self.evaluate(high).await != Status::Fail {
            return Err("the bad endpoint did not produce a stable FAIL");
        }
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            match self.evaluate(middle).await {
                Status::Pass => low = middle,
                Status::Fail => high = middle,
                Status::Incomplete | Status::Flaky => break,
            }
        }
        // Verify the prefix. A reversal may hide an earlier regression, and a
        // skipped or inconclusive revision cannot certify any later first failure.
        for index in 1..=high {
            match self.evaluate(index).await {
                Status::Pass => (),
                Status::Fail => return Ok(self.revisions[index].clone()),
                Status::Incomplete | Status::Flaky => {
                    return Err("a revision before the first verified failure is unresolved");
                }
            }
        }
        Err("no verified failure in the range")
    }
}

pub async fn run(host: &Supervisor, path: &Path) -> io::Result<Report> {
    host.require_control()?;
    let mut plan: Plan = serde_json::from_slice(&bounded_file(path, 32 << 10)?)?;
    if plan.version != 1
        || !identifier(&plan.good, &[40, 64])
        || !identifier(&plan.bad, &[plan.good.len()])
        || plan.good == plan.bad
        || !plan.git.is_absolute()
    {
        return Err(invalid(
            "bisect requires version 1, a local Git executable and distinct full revision IDs",
        ));
    }
    plan.git = plan.git.canonicalize()?;
    if !plan.git.is_file()
        || !plan
            .git
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return Err(invalid("bisect Git executable must be a Windows .exe"));
    }
    let repository = plan.repository.canonicalize()?;
    let common = common_dir(&plan.git, &repository).await?;
    let count = format!("--max-count={MAX_REVISIONS}");
    let history = git(
        &plan.git,
        &repository,
        &["rev-list", "--first-parent", &count, &plan.bad],
    )
    .await?;
    let mut revisions = Vec::new();
    for revision in history.lines() {
        revisions.push(revision.to_owned());
        if revision == plan.good {
            break;
        }
    }
    if revisions.first() != Some(&plan.bad)
        || revisions.last() != Some(&plan.good)
        || plan.profiles.len() != revisions.len()
        || revisions
            .iter()
            .any(|revision| !plan.profiles.contains_key(revision))
    {
        return Err(invalid(format!("supply one approved build profile for every first-parent revision in the range ({MAX_REVISIONS} maximum)")));
    }
    revisions.reverse();
    let mut roots = BTreeSet::new();
    let mut seconds = 0;
    for profile in plan.profiles.values() {
        let profile = host
            .config()
            .build_profiles
            .get(profile)
            .ok_or_else(|| invalid("bisect plan names an unapproved build profile"))?;
        if !roots.insert(&profile.root) {
            return Err(invalid("bisect profiles must use distinct worktrees"));
        }
        seconds += profile.timeout_secs;
    }
    let directory = path
        .canonicalize()?
        .parent()
        .expect("plan directory")
        .to_path_buf();
    let suite = suite::prepare(&directory.join(&plan.suite))?;
    if seconds + suite.seconds * revisions.len() as u64 > WALL_SECONDS
        || suite.cases.len() * usize::from(suite.repeats) * revisions.len() > MAX_EMULATION_ATTEMPTS
    {
        return Err(invalid(
            "bisect plan exceeds its aggregate wall or emulation attempt budget",
        ));
    }
    let output = host.config().storage_root.join(HISTORY_DIRECTORY);
    fs::create_dir_all(&output)?;
    if output.canonicalize()?.parent() != Some(host.config().storage_root.as_path()) {
        return Err(invalid(
            "regression history resolves outside the host storage root",
        ));
    }
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(output.join("bisect.lock"))?;
    lock.try_lock().map_err(io::Error::other)?;
    let quota = host.config().artifacts.max_bytes;
    let reserved = estimate(&suite, revisions.len())?;
    if reserved > MAX_REPORT_BYTES
        || regression_workspace_bytes(&output, quota)? > quota.saturating_sub(reserved)
        || reserved > quota
    {
        return Err(invalid(
            "bisect plan exceeds its retained evidence disk budget",
        ));
    }
    let worker_hash = execution::file_hash(&host.config().worker)?;
    let mut search = Search {
        host,
        plan,
        suite,
        common,
        revisions,
        output,
        worker_hash,
        quota,
        deadline: tokio::time::Instant::now() + Duration::from_secs(WALL_SECONDS),
        trials: BTreeMap::new(),
    };
    let outcome = search.first_bad().await;
    let id = SessionUuid::generate().to_string();
    let report = Report {
        version: 1,
        id: id.clone(),
        good: search.plan.good,
        bad: search.plan.bad,
        first_bad: outcome.as_ref().ok().cloned(),
        worker_sha256: search.worker_hash,
        reason: outcome.err(),
        trials: search.trials.into_values().collect(),
    };
    let pending = search.output.join(format!("bisect-{id}.tmp"));
    let file = File::create_new(&pending)?;
    serde_json::to_writer_pretty(&file, &report)?;
    file.sync_all()?;
    drop(file);
    fs::rename(pending, search.output.join(format!("bisect-{id}.json")))?;
    Ok(report)
}

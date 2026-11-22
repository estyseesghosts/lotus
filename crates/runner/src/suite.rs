//! Versioned regression suites built on retained scenario assessments.
use crate::{
    assessment::{self, Status},
    execution, Scenario,
};
use lotus_protocol::envelope::SessionUuid;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

const MAX_INPUT_BYTES: u64 = 128 << 10;
const MAX_REPORT_BYTES: u64 = 512 << 10;
const MAX_SUITE_SECONDS: u64 = 1800;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    repeats: u8,
    scenarios: Vec<PathBuf>,
}

pub(crate) struct PreparedSuite {
    pub(crate) repeats: u8,
    pub(crate) cases: Vec<(PathBuf, io::Result<(PathBuf, Scenario)>)>,
    pub(crate) seconds: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Case {
    pub scenario: PathBuf,
    pub status: Status,
    pub assessment: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    pub id: String,
    /// Source revision declared by the caller. Worker/content identity is measured per attempt.
    pub declared_revision: String,
    pub worker_sha256: String,
    pub status: Status,
    pub cases: Vec<Case>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub(crate) fn bounded_file(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid(format!("suite metadata exceeds {limit} bytes")));
    }
    Ok(bytes)
}

pub(crate) fn identifier(value: &str, lengths: &[usize]) -> bool {
    lengths.contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub async fn run(
    manifest_path: &Path,
    worker: &Path,
    output: &Path,
    revision: &str,
) -> io::Result<Report> {
    run_prepared(&prepare(manifest_path)?, worker, output, revision, None).await
}

pub(crate) fn prepare(manifest_path: &Path) -> io::Result<PreparedSuite> {
    let manifest: Manifest = serde_yaml::from_slice(&bounded_file(manifest_path, MAX_INPUT_BYTES)?)
        .map_err(io::Error::other)?;
    if manifest.version != 1
        || !(2..=10).contains(&manifest.repeats)
        || !(1..=16).contains(&manifest.scenarios.len())
    {
        return Err(invalid(
            "suite version 1 requires 1..=16 scenarios and 2..=10 repeats",
        ));
    }
    let directory = manifest_path
        .canonicalize()?
        .parent()
        .expect("manifest directory")
        .to_path_buf();
    let mut paths = BTreeSet::new();
    let mut seconds = 0u64;
    let mut prepared = Vec::with_capacity(manifest.scenarios.len());
    for relative in manifest.scenarios {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || !paths.insert(relative.clone())
        {
            return Err(invalid(
                "suite scenario paths must be unique and relative to the manifest directory",
            ));
        }
        let load = || {
            let path = directory.join(&relative).canonicalize()?;
            if !path.starts_with(&directory) {
                return Err(invalid(
                    "scenario path resolves outside the suite directory",
                ));
            }
            let scenario: Scenario = serde_yaml::from_slice(&bounded_file(&path, MAX_INPUT_BYTES)?)
                .map_err(io::Error::other)?;
            execution::validate_execution(&scenario)?;
            Ok((path, scenario))
        };
        let scenario = load();
        if let Ok((_, scenario)) = &scenario {
            seconds += scenario.stop.wall_timeout_seconds * u64::from(manifest.repeats);
        }
        prepared.push((relative, scenario));
    }
    if seconds > MAX_SUITE_SECONDS {
        return Err(invalid(
            "sum of scenario deadlines exceeds the suite's 1800-second budget",
        ));
    }
    Ok(PreparedSuite {
        repeats: manifest.repeats,
        cases: prepared,
        seconds,
    })
}

pub(crate) async fn run_prepared(
    suite: &PreparedSuite,
    worker: &Path,
    output: &Path,
    revision: &str,
    fixture: Option<(&Path, &str)>,
) -> io::Result<Report> {
    if !identifier(revision, &[40, 64]) {
        return Err(invalid(
            "suite revision must be a full lowercase Git object ID",
        ));
    }
    let worker = worker.canonicalize()?;
    let worker_sha256 = execution::file_hash(&worker)?;
    let id = SessionUuid::generate().to_string();
    fs::create_dir_all(output)?;
    let root = output.join(&id);
    fs::create_dir(&root)?;
    let mut cases = Vec::with_capacity(suite.cases.len());
    for (relative, scenario) in &suite.cases {
        let assessed = match scenario {
            Ok((path, scenario)) => {
                assessment::assess_fixture(
                    scenario,
                    path.parent().expect("scenario directory"),
                    &worker,
                    &root,
                    suite.repeats,
                    fixture,
                )
                .await
            }
            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
        };
        cases.push(match assessed {
            Ok(result) => {
                let worker_changed = result
                    .attempts
                    .iter()
                    .filter_map(|attempt| attempt.worker_sha256.as_deref())
                    .any(|observed| observed != worker_sha256);
                Case {
                    scenario: relative.clone(),
                    status: if worker_changed {
                        Status::Incomplete
                    } else {
                        result.status
                    },
                    assessment: Some(format!("{}/assessment.json", result.id)),
                    error: worker_changed
                        .then(|| "worker identity changed during the suite".into()),
                }
            }

            Err(error) => Case {
                scenario: relative.clone(),
                status: Status::Incomplete,
                assessment: None,
                error: Some(error.to_string().chars().take(512).collect()),
            },
        });
    }
    let status = if execution::file_hash(&worker).ok().as_deref() != Some(&worker_sha256) {
        Status::Incomplete
    } else {
        [Status::Incomplete, Status::Flaky, Status::Fail]
            .into_iter()
            .find(|status| cases.iter().any(|case| case.status == *status))
            .unwrap_or(Status::Pass)
    };
    let report = Report {
        version: 1,
        id,
        declared_revision: revision.into(),
        worker_sha256,
        status,
        cases,
    };
    let pending = root.join("suite.tmp");
    let file = File::create(&pending)?;
    serde_json::to_writer_pretty(&file, &report)?;
    file.sync_all()?;
    drop(file);
    fs::rename(pending, root.join("suite.json"))?;
    Ok(report)
}

pub fn show(output: &Path, id: &str) -> io::Result<Report> {
    if !identifier(id, &[32]) {
        return Err(invalid("expected a lowercase 32-digit suite ID"));
    }
    let output = output.canonicalize()?;
    let directory = output.join(id).canonicalize()?;
    let path = directory.join("suite.json").canonicalize()?;
    if directory.parent() != Some(output.as_path()) || path.parent() != Some(directory.as_path()) {
        return Err(invalid(
            "suite report resolves outside its history directory",
        ));
    }
    let report: Report = serde_json::from_slice(&bounded_file(&path, MAX_REPORT_BYTES)?)?;
    if report.version != 1
        || report.id != id
        || !identifier(&report.declared_revision, &[40, 64])
        || !identifier(&report.worker_sha256, &[64])
        || !(1..=16).contains(&report.cases.len())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "suite report identity mismatch",
        ));
    }
    Ok(report)
}

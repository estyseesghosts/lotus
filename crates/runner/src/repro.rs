//! Export verified suite comparisons and the scenario inputs needed to replay a regression.
use crate::{
    assessment::{digest, Status},
    execution::validate_execution,
    history::{self, Comparison, Detail},
    suite::{bounded_file, identifier, MAX_RECIPE_BYTES},
    Scenario,
};
use serde::Serialize;
use std::{
    fmt::Write as _,
    fs,
    io::{self, Write as _},
    path::Path,
};

const MAX_EXPORT_BYTES: usize = 8 << 20;
const REPLAY_SCRIPT: &str = r#"param(
    [Parameter(Mandatory = $true)][string]$Cli,
    [Parameter(Mandatory = $true)][string]$Worker,
    [Parameter(Mandatory = $true)][string]$Output,
    [Parameter(Mandatory = $true)][ValidateRange(1, 32)][int]$Case,
    [Parameter(Mandatory = $true)][ValidateSet('baseline', 'candidate')][string]$Side
)
$ErrorActionPreference = 'Stop'
$report = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'report.json') | ConvertFrom-Json
$entry = $report.replays | Where-Object { $_.case -eq $Case }
$recipe = $entry.$Side
if ($null -eq $recipe) { throw "No replay is available for case $Case." }
$scenario = Join-Path $PSScriptRoot ('case-{0:00}-{1}.json' -f $Case, $Side.ToLowerInvariant())
& $Cli scenario assess $scenario $Worker $Output $recipe.repeats
exit $LASTEXITCODE
"#;

#[derive(Serialize)]
struct Side<'a> {
    revision: &'a str,
    worker_sha256: &'a str,
}

#[derive(Serialize)]
struct Recipe<'a> {
    scenario: String,
    original_scenario_sha256: &'a str,
    content: String,
    content_sha256: &'a str,
    repeats: usize,
}

#[derive(Serialize)]
struct Replay<'a> {
    /// One-based index into comparison.cases.
    case: usize,
    baseline: Option<Recipe<'a>>,
    candidate: Option<Recipe<'a>>,
    unavailable: Option<&'static str>,
}

#[derive(Serialize)]
struct Report<'a> {
    version: u32,
    baseline: Side<'a>,
    candidate: Side<'a>,
    comparison: Comparison,
    replays: Vec<Replay<'a>>,
}

fn side(detail: &Detail) -> Side<'_> {
    Side {
        revision: &detail.suite.declared_revision,
        worker_sha256: &detail.suite.worker_sha256,
    }
}

fn snapshot<'a>(
    root: &Path,
    detail: &'a Detail,
    scenario_path: &Path,
    stem: &str,
) -> Result<(Recipe<'a>, Vec<u8>), &'static str> {
    let row = detail
        .cases
        .iter()
        .find(|row| row.scenario == scenario_path)
        .ok_or("case unavailable")?;
    let retained = row
        .retained
        .as_ref()
        .ok_or("verified repeats unavailable")?;
    let scenario_hash = row
        .scenario_sha256
        .as_deref()
        .ok_or("scenario hash unavailable")?;
    let content_hash = row
        .content_sha256
        .as_deref()
        .filter(|hash| identifier(hash, &[64]))
        .ok_or("fixture hash unavailable")?;
    let suite_directory = root.join(&detail.suite.id);
    let directory = suite_directory.join(&retained.assessment);
    let path = directory
        .join("scenario.json")
        .canonicalize()
        .map_err(|_| "retained scenario unavailable")?;
    if path.parent() != Some(directory.as_path()) {
        return Err("retained scenario resolves outside its assessment");
    }
    let bytes =
        bounded_file(&path, MAX_RECIPE_BYTES).map_err(|_| "retained scenario unreadable")?;
    if digest(&bytes) != scenario_hash {
        return Err("retained scenario SHA-256 mismatch");
    }
    let mut scenario: Scenario =
        serde_json::from_slice(&bytes).map_err(|_| "retained scenario invalid")?;
    if scenario
        .movie
        .as_ref()
        .is_some_and(|movie| movie.mode == "play")
    {
        return Err("movie playback requires a separate recording");
    }
    let content = format!("{stem}.3dsx");
    scenario.content.path_handle.clone_from(&content);
    scenario.content.sha256 = Some(content_hash.into());
    scenario.profile.worker_sha256 = Some(detail.suite.worker_sha256.clone());
    validate_execution(&scenario).map_err(|_| "retained scenario is not executable")?;
    let recipe = Recipe {
        scenario: format!("{stem}.json"),
        original_scenario_sha256: scenario_hash,
        content,
        content_sha256: content_hash,
        repeats: retained.repeats,
    };
    let bytes = serde_json::to_vec(&scenario).map_err(|_| "scenario serialization failed")?;
    Ok((recipe, bytes))
}

fn markdown(report: &Report<'_>) -> String {
    let mut text = format!(
        "# Suite regression comparison\n\nBaseline `{}` at `{}` with worker `{}`.\n\nCandidate `{}` at `{}` with worker `{}`.\n\n| Case | Scenario | Baseline | Candidate | Observation |\n| --- | --- | --- | --- | --- |\n",
        report.comparison.baseline, report.baseline.revision, report.baseline.worker_sha256,
        report.comparison.candidate, report.candidate.revision, report.candidate.worker_sha256,
    );
    for (index, case) in report.comparison.cases.iter().enumerate() {
        let status = |value| match value {
            Some(Status::Pass) => "PASS",
            Some(Status::Fail) => "FAIL",
            Some(Status::Flaky) => "FLAKY",
            Some(Status::Incomplete) => "INCOMPLETE",
            None => "—",
        };
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} | {} |",
            index + 1,
            markdown_code(&case.scenario.to_string_lossy()),
            status(case.baseline),
            status(case.candidate),
            case.observation
        );
    }
    text.push('\n');
    for (index, case) in report.comparison.cases.iter().enumerate() {
        if let Some(failure) = &case.first_failure {
            let _ = writeln!(
                text,
                "- Case {} {} assertion expected {} and observed {}.",
                index + 1,
                markdown_code(&failure.kind),
                markdown_code(&failure.expected.to_string()),
                markdown_code(&failure.actual.to_string())
            );
        }
        if let Some(point) = &case.checkpoint {
            let _ = writeln!(
                text,
                "- Case {} {} checkpoint at frame {} changed from {} to {}.",
                index + 1,
                markdown_code(&point.screen),
                point.frame,
                markdown_code(point.baseline_sha256.as_deref().unwrap_or("missing")),
                markdown_code(point.candidate_sha256.as_deref().unwrap_or("missing"))
            );
        }
    }
    text.push_str("\nSee `report.json` for the complete structured comparison.\n");
    for replay in &report.replays {
        let _ = writeln!(text, "\n## Replay case {}\n", replay.case);
        for (name, recipe) in [
            ("baseline", &replay.baseline),
            ("candidate", &replay.candidate),
        ] {
            if let Some(recipe) = recipe {
                let _ = writeln!(
                    text,
                    "Place your {name} fixture at `{}` with SHA-256 `{}` and use the corresponding worker listed above.\n\n```powershell\n.\\replay.ps1 -Cli '.\\ctr-cli.exe' -Worker '.\\ctr-worker.exe' -Output '.\\{name}-attempts' -Case {} -Side {name}\n```\n",
                    recipe.content, recipe.content_sha256, replay.case,
                );
            }
        }
        if let Some(reason) = replay.unavailable {
            let _ = writeln!(text, "Replay unavailable ({reason}).\n");
        }
    }
    text
}

fn markdown_code(value: &str) -> String {
    let value = value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "&#124;")
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    format!("<code>{value}</code>")
}

/// Export a comparison and replay recipes for stable PASS-to-FAIL cases.
/// Only metadata is written. Source content and executables are never opened.
pub fn export(root: &Path, baseline: &str, candidate: &str, output: &Path) -> io::Result<()> {
    let root = root.canonicalize()?;
    let before = history::detail(&root, baseline)?;
    let after = history::detail(&root, candidate)?;
    let mut report = Report {
        version: 1,
        baseline: side(&before),
        candidate: side(&after),
        comparison: history::compare_details(&before, &after),
        replays: Vec::new(),
    };
    let mut files = Vec::new();
    let mut bytes = 0;
    let mut add = |name: String, file: Vec<u8>| -> io::Result<()> {
        bytes += file.len();
        if bytes > MAX_EXPORT_BYTES {
            return Err(io::Error::other("regression export exceeds 8 MiB"));
        }
        files.push((name, file));
        Ok(())
    };
    for (index, change) in report.comparison.cases.iter().enumerate() {
        if (change.baseline, change.candidate, change.observation)
            != (Some(Status::Pass), Some(Status::Fail), "compared")
        {
            continue;
        }
        let case = index + 1;
        let mut replay = Replay {
            case,
            baseline: None,
            candidate: None,
            unavailable: None,
        };
        match (
            snapshot(
                &root,
                &before,
                &change.scenario,
                &format!("case-{case:02}-baseline"),
            ),
            snapshot(
                &root,
                &after,
                &change.scenario,
                &format!("case-{case:02}-candidate"),
            ),
        ) {
            (Ok((baseline, before)), Ok((candidate, after))) => {
                add(baseline.scenario.clone(), before)?;
                add(candidate.scenario.clone(), after)?;
                replay.baseline = Some(baseline);
                replay.candidate = Some(candidate);
            }
            (Err(reason), _) | (_, Err(reason)) => replay.unavailable = Some(reason),
        }
        report.replays.push(replay);
    }
    if report
        .replays
        .iter()
        .any(|replay| replay.baseline.is_some())
    {
        add("replay.ps1".into(), REPLAY_SCRIPT.as_bytes().to_vec())?;
    }
    add("README.md".into(), markdown(&report).into_bytes())?;
    add("report.json".into(), serde_json::to_vec_pretty(&report)?)?;

    fs::create_dir(output)?;
    let mut created = Vec::new();
    let result = (|| {
        for (name, bytes) in files {
            let path = output.join(name);
            let mut file = fs::File::create_new(&path)?;
            created.push(path);
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        for path in created {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_dir(output);
    }
    result
}

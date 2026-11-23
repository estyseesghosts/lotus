//! Read retained suite results without opening a worker or loading guest assets.
use crate::{
    assertions::AssertionResult,
    assessment::{self, Assessment, Status},
    execution::{Checkpoint, RunReport},
    suite::{self, bounded_file, identifier},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

const MAX_METADATA: u64 = 512 << 10;
pub const HISTORY_DIRECTORY: &str = "regressions";

#[derive(Debug, Serialize)]
pub struct Entry {
    pub id: String,
    pub declared_revision: String,
    pub worker_sha256: String,
    pub status: Status,
    pub case_count: usize,
}

#[derive(Debug, Serialize)]
pub struct CaseDetails {
    pub scenario: PathBuf,
    pub status: Status,
    pub scenario_sha256: Option<String>,
    pub content_sha256: Option<String>,
    pub first_failure: Option<AssertionResult>,
    pub checkpoints: Vec<Checkpoint>,
    pub error: Option<String>,
    #[serde(skip)]
    assertions: Vec<AssertionResult>,
    #[serde(skip)]
    pub(crate) retained: Option<RetainedRecipe>,
}

#[derive(Debug)]
pub(crate) struct RetainedRecipe {
    pub(crate) assessment: String,
    pub(crate) repeats: usize,
}

#[derive(Debug, Serialize)]
pub struct Detail {
    pub suite: suite::Report,
    pub cases: Vec<CaseDetails>,
}

#[derive(Debug, Serialize)]
pub struct Difference {
    pub scenario: PathBuf,
    pub baseline: Option<Status>,
    pub candidate: Option<Status>,
    pub observation: &'static str,
    pub checkpoint: Option<CheckpointDifference>,
    pub first_failure: Option<AssertionResult>,
    pub assertion_change: Option<AssertionDifference>,
}

#[derive(Debug, Serialize)]
pub struct AssertionDifference {
    pub baseline: AssertionResult,
    pub candidate: AssertionResult,
}

#[derive(Debug, Serialize)]
pub struct CheckpointDifference {
    pub frame: u64,
    pub screen: String,
    pub trigger: Option<String>,
    pub baseline_sha256: Option<String>,
    pub candidate_sha256: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub baseline: String,
    pub candidate: String,
    pub cases: Vec<Difference>,
}

/// List up to 32 completed suites, newest first. In-progress directories have no suite.json.
pub fn list(root: &Path) -> io::Result<Vec<Entry>> {
    let directory = match fs::read_dir(root) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut recent = BTreeSet::new();
    for entry in directory {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if !identifier(&id, &[32]) || !entry.file_type()?.is_dir() {
            continue;
        }
        let Ok(timestamp) = entry.path().join("suite.json").metadata() else {
            continue;
        };
        recent.insert((timestamp.modified().unwrap_or(SystemTime::UNIX_EPOCH), id));
        if recent.len() > 32 {
            recent.pop_first();
        }
    }
    recent
        .into_iter()
        .rev()
        .map(|(_, id)| {
            let report = suite::show(root, &id)?;
            Ok(Entry {
                id,
                declared_revision: report.declared_revision,
                worker_sha256: report.worker_sha256,
                status: report.status,
                case_count: report.cases.len(),
            })
        })
        .collect()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_metadata<T: serde::de::DeserializeOwned>(root: &Path, path: &Path) -> io::Result<T> {
    let path = path.canonicalize()?;
    if !path.starts_with(root) {
        return Err(invalid("report path resolves outside the suite directory"));
    }
    Ok(serde_json::from_slice(&bounded_file(&path, MAX_METADATA)?)?)
}

fn observations(
    root: &Path,
    case: &suite::Case,
    worker_sha256: &str,
) -> io::Result<(Assessment, Option<RunReport>)> {
    let path = case
        .assessment
        .as_deref()
        .ok_or_else(|| invalid("case has no retained assessment"))?;
    let id = path
        .strip_suffix("/assessment.json")
        .filter(|id| identifier(id, &[32]))
        .ok_or_else(|| invalid("invalid assessment reference"))?;
    let assessment: Assessment = read_metadata(root, &root.join(path))?;
    if assessment.id != id
        || assessment.version != 1
        || !(2..=10).contains(&assessment.attempts.len())
    {
        return Err(invalid("assessment identity mismatch"));
    }
    let attempt = &assessment.attempts[0];
    let report = if matches!(case.status, Status::Pass | Status::Fail) {
        let fingerprint = attempt
            .observation_sha256
            .as_deref()
            .ok_or_else(|| invalid("assessment is missing an observation fingerprint"))?;
        if assessment.status != case.status {
            return Err(invalid("assessment lacks matching repeated observations"));
        }
        let mut first = None;
        for (index, observed) in assessment.attempts.iter().enumerate() {
            let path = format!("attempt-{:02}/report.json", index + 1);
            if usize::from(observed.number) != index + 1
                || observed.report.as_deref() != Some(&path)
                || observed.status != case.status
                || observed.observation_sha256.as_deref() != Some(fingerprint)
                || observed.worker_sha256.as_deref() != Some(worker_sha256)
            {
                return Err(invalid("assessment lacks matching repeated observations"));
            }
            let report: RunReport = read_metadata(root, &root.join(id).join(path))?;
            if report.worker_executable_sha256 != worker_sha256
                || assessment::outcome(&report) != case.status
                || assessment::fingerprint(&report)? != fingerprint
            {
                return Err(invalid("attempt observation identity mismatch"));
            }
            if index == 0 {
                first = Some(report);
            }
        }
        first
    } else {
        None
    };
    Ok((assessment, report))
}

pub fn detail(output: &Path, id: &str) -> io::Result<Detail> {
    let suite = suite::show(output, id)?;
    let root = output.join(id).canonicalize()?;
    let mut cases = Vec::with_capacity(suite.cases.len());
    for case in &suite.cases {
        let mut row = CaseDetails {
            scenario: case.scenario.clone(),
            status: case.status,
            scenario_sha256: None,
            content_sha256: None,
            first_failure: None,
            checkpoints: Vec::new(),
            error: case.error.clone(),
            assertions: Vec::new(),
            retained: None,
        };
        if case.assessment.is_some() {
            match observations(&root, case, &suite.worker_sha256) {
                Ok((assessment, report)) => {
                    row.scenario_sha256 = Some(assessment.scenario_sha256);
                    if let Some(report) = report {
                        row.retained = Some(RetainedRecipe {
                            assessment: assessment.id,
                            repeats: assessment.attempts.len(),
                        });
                        row.content_sha256 = Some(report.content_sha256);
                        row.checkpoints = report.checkpoints;
                        row.first_failure = report
                            .assertions
                            .iter()
                            .find(|result| result.outcome != crate::assertions::Outcome::Pass)
                            .cloned();
                        row.assertions = report.assertions;
                    } else {
                        if row.error.is_none() {
                            row.error = assessment
                                .attempts
                                .iter()
                                .find_map(|attempt| attempt.error.clone());
                        }
                        row.first_failure = assessment
                            .attempts
                            .into_iter()
                            .find_map(|attempt| attempt.first_failure);
                    }
                }
                Err(error) => {
                    row.status = Status::Incomplete;
                    row.error = Some(error.to_string());
                }
            }
        } else {
            row.status = Status::Incomplete;
        }
        cases.push(row);
    }
    Ok(Detail { suite, cases })
}

fn checkpoint_difference(
    baseline: &[Checkpoint],
    candidate: &[Checkpoint],
) -> Option<CheckpointDifference> {
    fn index(checkpoints: &[Checkpoint]) -> BTreeMap<(u64, &str, Option<&str>), &str> {
        checkpoints
            .iter()
            .map(|capture| {
                (
                    (
                        capture.frame,
                        capture.screen.as_str(),
                        capture.trigger.as_deref(),
                    ),
                    capture.sha256.as_str(),
                )
            })
            .collect()
    }
    let baseline = index(baseline);
    let candidate = index(candidate);
    let keys: BTreeSet<_> = baseline.keys().chain(candidate.keys()).collect();
    for &(frame, screen, trigger) in keys {
        let key = (frame, screen, trigger);
        let before = baseline.get(&key);
        let after = candidate.get(&key);
        if before != after {
            return Some(CheckpointDifference {
                frame,
                screen: screen.into(),
                trigger: trigger.map(str::to_owned),
                baseline_sha256: before.map(|hash| (*hash).into()),
                candidate_sha256: after.map(|hash| (*hash).into()),
            });
        }
    }
    None
}

pub fn compare(root: &Path, baseline: &str, candidate: &str) -> io::Result<Comparison> {
    let before = detail(root, baseline)?;
    let after = detail(root, candidate)?;
    Ok(compare_details(&before, &after))
}

pub(crate) fn compare_details(before: &Detail, after: &Detail) -> Comparison {
    let mut later: BTreeMap<_, _> = after
        .cases
        .iter()
        .map(|row| (row.scenario.clone(), row))
        .collect();
    let mut cases = Vec::new();
    for row in &before.cases {
        let next = later.remove(&row.scenario);
        let mut checkpoint = None;
        let mut assertion_change = None;
        let observation = match &next {
            None => "removed",
            Some(next)
                if ![row.status, next.status]
                    .iter()
                    .all(|status| matches!(status, Status::Pass | Status::Fail)) =>
            {
                "inconclusive"
            }
            Some(next) if row.scenario_sha256 != next.scenario_sha256 => "scenario_changed",
            Some(next) => {
                checkpoint = checkpoint_difference(&row.checkpoints, &next.checkpoints);
                assertion_change = row
                    .assertions
                    .iter()
                    .zip(&next.assertions)
                    .find(|(old, new)| {
                        old.index == new.index
                            && old.kind == new.kind
                            && (old.actual != new.actual || old.outcome != new.outcome)
                    })
                    .map(|(old, new)| AssertionDifference {
                        baseline: old.clone(),
                        candidate: new.clone(),
                    });
                "compared"
            }
        };
        cases.push(Difference {
            scenario: row.scenario.clone(),
            baseline: Some(row.status),
            candidate: next.map(|next| next.status),
            observation,
            checkpoint,
            first_failure: next.and_then(|next| next.first_failure.clone()),
            assertion_change,
        });
    }
    cases.extend(later.into_values().map(|row| Difference {
        scenario: row.scenario.clone(),
        baseline: None,
        candidate: Some(row.status),
        observation: "added",
        checkpoint: None,
        first_failure: row.first_failure.clone(),
        assertion_change: None,
    }));
    Comparison {
        baseline: before.suite.id.clone(),
        candidate: after.suite.id.clone(),
        cases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(frame: u64, screen: &str, trigger: Option<&str>, hash: &str) -> Checkpoint {
        Checkpoint {
            frame,
            screen: screen.into(),
            trigger: trigger.map(str::to_owned),
            sha256: hash.into(),
            bytes: 0,
            artifact: String::new(),
        }
    }

    #[test]
    fn checkpoint_differences_use_frame_screen_and_trigger() {
        let frame = capture(1, "top", None, "same");
        let event = capture(1, "top", Some("guest_log"), "first");
        let different_event = capture(1, "top", Some("guest_log"), "second");
        let later = capture(2, "bottom", None, "later");
        let before = [later.clone(), frame.clone(), event.clone()];
        let after = [frame.clone(), different_event];
        let change = checkpoint_difference(&before, &after).unwrap();
        assert_eq!(change.frame, 1);
        assert_eq!(change.trigger.as_deref(), Some("guest_log"));
        assert_eq!(change.baseline_sha256.as_deref(), Some("first"));
        assert_eq!(change.candidate_sha256.as_deref(), Some("second"));
        let change = checkpoint_difference(&before, &[frame, event]).unwrap();
        assert_eq!(change.frame, later.frame);
        assert!(change.candidate_sha256.is_none());
        assert!(checkpoint_difference(&before, &before).is_none());
    }
}

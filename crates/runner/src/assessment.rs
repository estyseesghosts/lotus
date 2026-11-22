//! Retained, bounded repeat assessment for one scenario and worker.
use crate::{
    assertions::{AssertionResult, Outcome},
    execution::{self, RunReport},
    Scenario,
};
use lotus_protocol::envelope::SessionUuid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Pass,
    Fail,
    Incomplete,
    Flaky,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub number: u8,
    pub status: Status,
    pub report: Option<String>,
    pub first_failure: Option<AssertionResult>,
    pub error: Option<String>,
    pub observation_sha256: Option<String>,
    pub worker_sha256: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Assessment {
    pub version: u32,
    pub id: String,
    pub name: String,
    pub scenario_sha256: String,
    pub status: Status,
    pub attempts: Vec<Attempt>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn outcome(report: &RunReport) -> Status {
    if report
        .assertions
        .iter()
        .any(|result| result.outcome == Outcome::Inconclusive)
        || (report.assertions.is_empty() && report.checkpoints.is_empty())
    {
        Status::Incomplete
    } else if report.passed {
        Status::Pass
    } else {
        Status::Fail
    }
}

pub(crate) fn fingerprint(report: &RunReport) -> io::Result<String> {
    let screens = report
        .checkpoints
        .iter()
        .map(|screen| {
            (
                &screen.trigger,
                screen.frame,
                &screen.screen,
                &screen.sha256,
            )
        })
        .collect::<Vec<_>>();
    let assertions = report
        .assertions
        .iter()
        .map(|result| {
            (
                result.index,
                &result.kind,
                result.outcome,
                &result.expected,
                &result.actual,
            )
        })
        .collect::<Vec<_>>();
    Ok(digest(&serde_json::to_vec(&(
        &report.worker_executable_sha256,
        &report.content_sha256,
        &report.source,
        report.frames,
        screens,
        assertions,
    ))?))
}

fn classify(attempts: &[Attempt]) -> Status {
    if attempts
        .iter()
        .any(|attempt| attempt.status == Status::Incomplete)
    {
        return Status::Incomplete;
    }
    let first = &attempts[0];
    if attempts.iter().skip(1).any(|attempt| {
        attempt.status != first.status || attempt.observation_sha256 != first.observation_sha256
    }) {
        Status::Flaky
    } else {
        first.status
    }
}

/// Preserve every attempt, including failed assertions and unavailable observations.
/// The assessment is published when all attempts conclude. In-flight attempts have
/// their own reports, so a partial assessment can never be mistaken for a result.
pub async fn assess(
    scenario: &Scenario,
    directory: &Path,
    worker: &Path,
    output: &Path,
    repeats: u8,
) -> io::Result<Assessment> {
    if !(2..=10).contains(&repeats) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "assess requires 2..=10 attempts",
        ));
    }
    execution::validate_execution(scenario)?;
    let id = SessionUuid::generate().to_string();
    fs::create_dir_all(output)?;
    let root = output.join(&id);
    fs::create_dir(&root)?;
    let mut attempts = Vec::with_capacity(repeats as usize);
    for number in 1..=repeats {
        let folder = format!("attempt-{number:02}");
        let attempt =
            match execution::run_report(scenario, directory, worker, &root.join(&folder)).await {
                Ok(report) => Attempt {
                    number,
                    status: outcome(&report),
                    report: Some(format!("{folder}/report.json")),
                    observation_sha256: Some(fingerprint(&report)?),
                    worker_sha256: Some(report.worker_executable_sha256),
                    first_failure: report
                        .assertions
                        .into_iter()
                        .find(|result| result.outcome != Outcome::Pass),
                    error: None,
                },
                Err(error) => Attempt {
                    number,
                    status: Status::Incomplete,
                    report: None,
                    observation_sha256: None,
                    worker_sha256: None,
                    first_failure: None,
                    error: Some(error.to_string()),
                },
            };
        attempts.push(attempt);
    }
    let assessment = Assessment {
        version: 1,
        id,
        name: scenario.name.clone(),
        scenario_sha256: digest(&serde_json::to_vec(scenario)?),
        status: classify(&attempts),
        attempts,
    };
    let pending = root.join("assessment.tmp");
    let file = fs::File::create(&pending)?;
    serde_json::to_writer_pretty(&file, &assessment)?;
    file.sync_all()?;
    drop(file);
    fs::rename(pending, root.join("assessment.json"))?;
    Ok(assessment)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(status: Status, fingerprint: &str) -> Attempt {
        Attempt {
            number: 1,
            status,
            report: None,
            first_failure: None,
            error: None,
            observation_sha256: Some(fingerprint.into()),
            worker_sha256: None,
        }
    }

    #[test]
    fn repeat_assessment_requires_matching_observations() {
        use Status::*;
        assert_eq!(classify(&[attempt(Pass, "a"), attempt(Pass, "a")]), Pass);
        assert_eq!(classify(&[attempt(Fail, "a"), attempt(Fail, "a")]), Fail);
        assert_eq!(classify(&[attempt(Fail, "a"), attempt(Fail, "b")]), Flaky);
        assert_eq!(classify(&[attempt(Pass, "a"), attempt(Fail, "a")]), Flaky);
        assert_eq!(
            classify(&[attempt(Fail, "a"), attempt(Incomplete, "")]),
            Incomplete
        );
    }
}

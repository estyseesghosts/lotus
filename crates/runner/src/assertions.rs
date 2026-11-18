//! Scenario assertion schema (version 2) and evaluation (lotus#94).
//!
//! Assertions are evaluated from observations the runner records during the run: memory and
//! register reads taken at exact frame checkpoints, captured frame hashes, the worker's event
//! history and execute-breakpoint hit counters. Every result is `pass`, `fail` or
//! `inconclusive`; only an all-`pass` run passes.
//!
//! Event history is CITnLO's `events` stream (azahar-experimental `debug.h` `Stream`). It keeps
//! 1024 events and reports `lost_total`, `pending_lost_total` and `history_gap`; any loss inside
//! the observation window makes every event assertion inconclusive, so overflow is never read as
//! absence. Coverage uses an execute breakpoint configured with `hit_after = u64::MAX`: the
//! worker increments `hits` before comparing against `hit_after` and skips the stop while
//! `hits < hit_after` (`debug.h` execute-breakpoint path), so the counter runs without stopping
//! the guest or recording events.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Assertion, Scenario, ScenarioError};

/// Worker event kinds an assertion or capture trigger may name, with the CITnLO history
/// category that records them (`debug.h` `CategoryOf`). `pica` is excluded: it is only built
/// while `gpu_configure` captures events, which scenarios do not configure.
pub const EVENT_KINDS: [(&str, &str); 7] = [
    ("guest_log", "log"),
    ("hio_stdout", "log"),
    ("native_log", "native_log"),
    ("stopped", "stopped"),
    ("svc_enter", "svc"),
    ("svc_exit", "svc"),
    ("ipc", "ipc"),
];

/// Assertion kinds of schema version 2.
pub const KINDS_V2: [&str; 10] = [
    "no_worker_crash",
    "frame_count",
    "memory",
    "register",
    "frame_hash",
    "event_seen",
    "no_event",
    "event_count",
    "reached",
    "not_reached",
];

/// Kinds that need CITnLO debug operations (event history or breakpoint counters).
pub fn needs_experimental(kind: &str) -> bool {
    matches!(
        kind,
        "event_seen" | "no_event" | "event_count" | "reached" | "not_reached"
    )
}

/// Maximum assertions per scenario.
pub const MAX_ASSERTIONS: usize = 256;
/// Maximum coverage assertions; the worker caps breakpoints at 64 (`debug.h` `breakpoint_set`).
pub const MAX_COVERAGE: usize = 64;
/// Maximum `text_contains` length.
pub const MAX_TEXT: usize = 256;

fn invalid(index: usize, message: impl std::fmt::Display) -> ScenarioError {
    ScenarioError::Invalid(format!("assertion {index}: {message}"))
}

/// History category of an event kind.
pub fn category(kind: &str) -> Option<&'static str> {
    EVENT_KINDS
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, category)| *category)
}

fn check_event(index: usize, event: Option<&str>, text: Option<&str>) -> Result<(), ScenarioError> {
    let event = event.ok_or_else(|| invalid(index, "needs an event kind"))?;
    if category(event).is_none() {
        return Err(invalid(
            index,
            format!("unknown event kind {event}; expected one of guest_log, hio_stdout, native_log, stopped, svc_enter, svc_exit, ipc"),
        ));
    }
    if text.is_some_and(|text| text.is_empty() || text.len() > MAX_TEXT) {
        return Err(invalid(index, "text_contains must be 1..256 bytes"));
    }
    Ok(())
}

fn check_bounds(index: usize, assertion: &Assertion) -> Result<(), ScenarioError> {
    match (assertion.equals, assertion.minimum, assertion.maximum) {
        (Some(_), None, None) => Ok(()),
        (None, minimum, maximum) if minimum.is_some() || maximum.is_some() => {
            if minimum.zip(maximum).is_some_and(|(low, high)| low > high) {
                Err(invalid(index, "minimum exceeds maximum"))
            } else {
                Ok(())
            }
        }
        _ => Err(invalid(index, "needs equals, or minimum and/or maximum")),
    }
}

fn check_frame(
    index: usize,
    scenario: &Scenario,
    frame: Option<u64>,
) -> Result<u64, ScenarioError> {
    let frame = frame.ok_or_else(|| invalid(index, "needs at_frame"))?;
    if frame == 0 || frame > scenario.stop.max_frames {
        return Err(invalid(index, "at_frame must be 1..=stop.max_frames"));
    }
    Ok(frame)
}

/// Deterministic validation of version 2 assertions and capture triggers.
pub fn validate(scenario: &Scenario) -> Result<(), ScenarioError> {
    if scenario.assertions.len() > MAX_ASSERTIONS {
        return Err(ScenarioError::Invalid(format!(
            "more than {MAX_ASSERTIONS} assertions"
        )));
    }
    let coverage = scenario
        .assertions
        .iter()
        .filter(|assertion| matches!(assertion.kind.as_str(), "reached" | "not_reached"))
        .count();
    if coverage > MAX_COVERAGE {
        return Err(ScenarioError::Invalid(format!(
            "more than {MAX_COVERAGE} reached/not_reached assertions (worker breakpoint cap)"
        )));
    }
    for (index, assertion) in scenario.assertions.iter().enumerate() {
        let a = assertion;
        match a.kind.as_str() {
            "no_worker_crash" => {}
            "frame_count" => {
                if a.minimum.is_none() {
                    return Err(invalid(index, "frame_count needs minimum"));
                }
            }
            "memory" => {
                check_frame(index, scenario, a.at_frame)?;
                let address = a.address.ok_or_else(|| invalid(index, "needs address"))?;
                let width = a.width.unwrap_or(4);
                if !matches!(width, 1 | 2 | 4) {
                    return Err(invalid(index, "width must be 1, 2 or 4"));
                }
                if u64::from(address) + u64::from(width) > 1 << 32 {
                    return Err(invalid(index, "read crosses the guest address space"));
                }
                check_bounds(index, a)?;
            }
            "register" => {
                check_frame(index, scenario, a.at_frame)?;
                if a.core.unwrap_or(0) > 3 {
                    return Err(invalid(index, "core must be 0..3"));
                }
                if a.register.is_none_or(|register| register > 16) {
                    return Err(invalid(index, "register must be 0..16 (16 = cpsr)"));
                }
                check_bounds(index, a)?;
            }
            "frame_hash" => {
                let frame = check_frame(index, scenario, a.at_frame)?;
                if !matches!(a.screen.as_deref(), Some("top" | "bottom")) {
                    return Err(invalid(index, "screen must be top or bottom"));
                }
                if a.sha256.as_ref().is_none_or(|hash| {
                    hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
                }) {
                    return Err(invalid(index, "sha256 must be 64 hex digits"));
                }
                let captured = frame == scenario.stop.max_frames
                    || scenario.captures.iter().any(|capture| {
                        capture.when.as_ref().and_then(|when| when.frame) == Some(frame)
                    });
                if !captured {
                    return Err(invalid(
                        index,
                        "frame_hash needs a frame capture at at_frame",
                    ));
                }
            }
            "event_seen" | "no_event" | "event_count" => {
                check_event(index, a.event.as_deref(), a.text_contains.as_deref())?;
                if a.kind == "event_count" {
                    check_bounds(index, a)?;
                }
                if a.kind == "event_seen" && a.count_at_least == Some(0) {
                    return Err(invalid(index, "count_at_least must be at least 1"));
                }
            }
            "reached" | "not_reached" => {
                if a.address.is_none() {
                    return Err(invalid(index, "needs address"));
                }
                if a.kind == "reached" && a.count_at_least == Some(0) {
                    return Err(invalid(index, "count_at_least must be at least 1"));
                }
            }
            "marker" => {
                return Err(invalid(
                    index,
                    "marker assertions need guest markers (lotus#83)",
                ))
            }
            "fault_receipt" => {
                return Err(invalid(
                    index,
                    "fault receipts need fault injection (lotus#92/#93)",
                ))
            }
            other => return Err(invalid(index, format!("unknown assertion type {other}"))),
        }
    }
    for (index, capture) in scenario.captures.iter().enumerate() {
        let Some(when) = &capture.when else {
            return Err(ScenarioError::Invalid(format!(
                "capture {index} has no trigger"
            )));
        };
        match (when.frame, when.event.as_deref()) {
            (Some(_), None) => {
                if when.text_contains.is_some() {
                    return Err(ScenarioError::Invalid(format!(
                        "capture {index}: text_contains needs an event trigger"
                    )));
                }
            }
            (None, Some(event)) => {
                check_event(index, Some(event), when.text_contains.as_deref())
                    .map_err(|error| ScenarioError::Invalid(format!("capture {error}")))?
            }
            _ => {
                return Err(ScenarioError::Invalid(format!(
                    "capture {index} needs exactly one of frame or event"
                )))
            }
        }
    }
    Ok(())
}

/// Outcome of one assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    /// The observation could not decide the assertion (lost events, missing read).
    Inconclusive,
}

/// Machine-readable result of one assertion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionResult {
    pub index: usize,
    #[serde(rename = "type")]
    pub kind: String,
    pub outcome: Outcome,
    pub expected: Value,
    pub actual: Value,
    /// Where the observation came from.
    pub provenance: String,
}

/// An event filter: kind plus optional substring of `text`, `message` or `reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFilter {
    pub kind: String,
    pub text: Option<String>,
}

impl EventFilter {
    pub fn matches(&self, event: &Value) -> bool {
        if event["kind"].as_str() != Some(self.kind.as_str()) {
            return false;
        }
        let Some(needle) = &self.text else {
            return true;
        };
        ["text", "message", "reason"]
            .iter()
            .filter_map(|field| event[*field].as_str())
            .any(|text| text.contains(needle.as_str()))
    }
}

/// Matches seen for one filter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FilterCount {
    pub count: u64,
    pub first_sequence: Option<u64>,
    pub first_frame: Option<u64>,
}

/// Bounded accumulation of the worker's event history over the run. Only per-filter counts are
/// kept, not the events themselves.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventLedger {
    /// Last sequence consumed.
    pub after: u64,
    /// Events consumed in the window.
    pub observed: u64,
    /// Why the window is incomplete, if it is.
    pub loss: Option<String>,
    pub counts: Vec<FilterCount>,
    #[serde(skip)]
    pub filters: Vec<EventFilter>,
}

impl EventLedger {
    pub fn new(filters: Vec<EventFilter>) -> Self {
        Self {
            counts: vec![FilterCount::default(); filters.len()],
            filters,
            ..Self::default()
        }
    }

    /// Absorbs one `events` reply observed at `frame`. Returns `true` while more pages remain.
    pub fn absorb(&mut self, reply: &Value, frame: u64) -> bool {
        let lost = reply["lost_total"].as_u64().unwrap_or(u64::MAX);
        let pending = reply["pending_lost_total"].as_u64().unwrap_or(u64::MAX);
        let gap = reply["history_gap"].as_bool().unwrap_or(true);
        if (lost != 0 || pending != 0 || gap) && self.loss.is_none() {
            self.loss = Some(format!(
                "worker history lost events before frame {frame} (lost_total {lost}, pending_lost_total {pending}, history_gap {gap})"
            ));
        }
        let events = reply["events"].as_array().cloned().unwrap_or_default();
        for event in &events {
            let Some(sequence) = event["sequence"].as_u64() else {
                self.loss
                    .get_or_insert_with(|| "event without sequence".into());
                continue;
            };
            if sequence <= self.after {
                continue;
            }
            self.after = sequence;
            self.observed += 1;
            for (filter, count) in self.filters.iter().zip(&mut self.counts) {
                if filter.matches(event) {
                    count.count += 1;
                    count.first_sequence.get_or_insert(sequence);
                    count.first_frame.get_or_insert(frame);
                }
            }
        }
        let latest = reply["latest"].as_u64().unwrap_or(0);
        !events.is_empty() && self.after < latest
    }

    fn filter_index(&self, filter: &EventFilter) -> Option<usize> {
        self.filters
            .iter()
            .position(|candidate| candidate == filter)
    }

    /// Count for `filter`, if the ledger tracks it.
    pub fn count(&self, filter: &EventFilter) -> Option<&FilterCount> {
        self.filter_index(filter).map(|index| &self.counts[index])
    }
}

/// Filter of an event assertion.
pub fn assertion_filter(assertion: &Assertion) -> Option<EventFilter> {
    matches!(
        assertion.kind.as_str(),
        "event_seen" | "no_event" | "event_count"
    )
    .then(|| EventFilter {
        kind: assertion.event.clone().unwrap_or_default(),
        text: assertion.text_contains.clone(),
    })
}

fn bounds_hold(assertion: &Assertion, value: u64) -> bool {
    match assertion.equals {
        Some(expected) => value == expected,
        None => {
            assertion.minimum.is_none_or(|minimum| value >= minimum)
                && assertion.maximum.is_none_or(|maximum| value <= maximum)
        }
    }
}

fn expected_bounds(assertion: &Assertion) -> Value {
    match assertion.equals {
        Some(value) => serde_json::json!({"equals": value}),
        None => serde_json::json!({"minimum": assertion.minimum, "maximum": assertion.maximum}),
    }
}

/// Everything the runner observed, for evaluation after the run.
#[derive(Debug, Default)]
pub struct Observations {
    pub frames: u64,
    /// Values read for memory/register assertions, by assertion index.
    pub values: std::collections::BTreeMap<usize, u64>,
    /// `(frame, screen, sha256)` of every frame-triggered capture.
    pub hashes: Vec<(u64, String, String)>,
    pub events: Option<EventLedger>,
    /// Breakpoint hits for coverage assertions, by assertion index.
    pub hits: std::collections::BTreeMap<usize, u64>,
}

/// Evaluates every assertion from the run's observations.
pub fn evaluate(scenario: &Scenario, seen: &Observations) -> Vec<AssertionResult> {
    scenario
        .assertions
        .iter()
        .enumerate()
        .map(|(index, a)| {
            let (outcome, expected, actual, provenance) = match a.kind.as_str() {
                "no_worker_crash" => (
                    Outcome::Pass,
                    Value::from("worker alive"),
                    Value::from("worker answered status after the final frame"),
                    "status".to_string(),
                ),
                "frame_count" => {
                    let minimum = a.minimum.unwrap_or(0);
                    (
                        if seen.frames >= minimum {
                            Outcome::Pass
                        } else {
                            Outcome::Fail
                        },
                        serde_json::json!({"minimum": minimum}),
                        Value::from(seen.frames),
                        "advance frame counter".to_string(),
                    )
                }
                "memory" | "register" => {
                    let provenance = if a.kind == "memory" {
                        format!(
                            "read_memory 0x{:08x} width {} at frame {}",
                            a.address.unwrap_or(0),
                            a.width.unwrap_or(4),
                            a.at_frame.unwrap_or(0)
                        )
                    } else {
                        format!(
                            "registers core {} r{} at frame {}",
                            a.core.unwrap_or(0),
                            a.register.unwrap_or(0),
                            a.at_frame.unwrap_or(0)
                        )
                    };
                    match seen.values.get(&index) {
                        Some(value) => (
                            if bounds_hold(a, *value) {
                                Outcome::Pass
                            } else {
                                Outcome::Fail
                            },
                            expected_bounds(a),
                            Value::from(*value),
                            provenance,
                        ),
                        None => (
                            Outcome::Inconclusive,
                            expected_bounds(a),
                            Value::Null,
                            provenance,
                        ),
                    }
                }
                "frame_hash" => {
                    let frame = a.at_frame.unwrap_or(0);
                    let screen = a.screen.clone().unwrap_or_default();
                    let actual = seen
                        .hashes
                        .iter()
                        .find(|(at, name, _)| *at == frame && *name == screen)
                        .map(|(_, _, hash)| hash.clone());
                    let expected = a.sha256.clone().unwrap_or_default().to_ascii_lowercase();
                    (
                        match &actual {
                            Some(hash) if *hash == expected => Outcome::Pass,
                            Some(_) => Outcome::Fail,
                            None => Outcome::Inconclusive,
                        },
                        Value::from(expected),
                        actual.map_or(Value::Null, Value::from),
                        format!("{screen} capture at frame {frame}"),
                    )
                }
                "event_seen" | "no_event" | "event_count" => {
                    let filter = assertion_filter(a).expect("event assertion");
                    let expected = match a.kind.as_str() {
                        "event_seen" => {
                            serde_json::json!({"count_at_least": a.count_at_least.unwrap_or(1)})
                        }
                        "no_event" => serde_json::json!({"count": 0}),
                        _ => expected_bounds(a),
                    };
                    let provenance = format!(
                        "worker event history {} from load to frame {}",
                        filter.kind, seen.frames
                    );
                    match seen
                        .events
                        .as_ref()
                        .and_then(|ledger| ledger.count(&filter).map(|count| (ledger, count)))
                    {
                        None => (Outcome::Inconclusive, expected, Value::Null, provenance),
                        Some((ledger, count)) => {
                            let actual = serde_json::json!({
                                "count": count.count,
                                "first_sequence": count.first_sequence,
                                "first_frame": count.first_frame,
                                "loss": ledger.loss,
                            });
                            let holds = match a.kind.as_str() {
                                "event_seen" => count.count >= a.count_at_least.unwrap_or(1),
                                "no_event" => count.count == 0,
                                _ => bounds_hold(a, count.count),
                            };
                            let outcome = if ledger.loss.is_some() {
                                Outcome::Inconclusive
                            } else if holds {
                                Outcome::Pass
                            } else {
                                Outcome::Fail
                            };
                            (outcome, expected, actual, provenance)
                        }
                    }
                }
                "reached" | "not_reached" => {
                    let need = a.count_at_least.unwrap_or(1);
                    let expected = if a.kind == "reached" {
                        serde_json::json!({"hits_at_least": need})
                    } else {
                        serde_json::json!({"hits": 0})
                    };
                    let provenance = format!(
                        "execute breakpoint hit counter at 0x{:08x}",
                        a.address.unwrap_or(0)
                    );
                    match seen.hits.get(&index) {
                        None => (Outcome::Inconclusive, expected, Value::Null, provenance),
                        Some(hits) => {
                            let holds = if a.kind == "reached" {
                                *hits >= need
                            } else {
                                *hits == 0
                            };
                            (
                                if holds { Outcome::Pass } else { Outcome::Fail },
                                expected,
                                Value::from(*hits),
                                provenance,
                            )
                        }
                    }
                }
                other => (
                    Outcome::Inconclusive,
                    Value::Null,
                    Value::Null,
                    format!("no evaluator for {other}"),
                ),
            };
            AssertionResult {
                index,
                kind: a.kind.clone(),
                outcome,
                expected,
                actual,
                provenance,
            }
        })
        .collect()
}

/// Result for an event capture trigger that never fired: a failed promise, not a skipped
/// capture. With lost events the trigger may have fired unseen, so it is inconclusive.
pub fn unfired_trigger(
    index: usize,
    capture: usize,
    filter: &EventFilter,
    lost: bool,
) -> AssertionResult {
    AssertionResult {
        index,
        kind: "capture_trigger".into(),
        outcome: if lost {
            Outcome::Inconclusive
        } else {
            Outcome::Fail
        },
        expected: serde_json::json!({"event": filter.kind, "text_contains": filter.text}),
        actual: Value::from("not observed by stop.max_frames"),
        provenance: format!("capture {capture} event trigger"),
    }
}

/// One line per assertion for `report.txt`.
pub fn render(results: &[AssertionResult]) -> String {
    let mut text = String::new();
    for result in results {
        text.push_str(&format!(
            "{:>3} {:<13} {:<12} expected {} actual {} ({})\n",
            result.index,
            result.kind,
            format!("{:?}", result.outcome).to_ascii_uppercase(),
            result.expected,
            result.actual,
            result.provenance
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Scenario {
        Scenario::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../specs/scenario-example.yaml"),
        )
        .expect("example scenario")
    }

    fn page(events: Value, lost: u64, pending: u64, gap: bool, latest: u64) -> Value {
        serde_json::json!({"events": events, "lost_total": lost, "pending_lost_total": pending,
            "history_gap": gap, "latest": latest})
    }

    fn ready(sequence: u64) -> Value {
        serde_json::json!({"kind": "guest_log", "text": "lotus-fixture:ready\n", "sequence": sequence})
    }

    fn observed(scenario: &Scenario, ledger: EventLedger) -> Vec<AssertionResult> {
        let seen = Observations {
            frames: 60,
            events: Some(ledger),
            ..Observations::default()
        };
        evaluate(scenario, &seen)
            .into_iter()
            .filter(|result| matches!(result.kind.as_str(), "event_seen" | "no_event"))
            .collect()
    }

    fn ledger(scenario: &Scenario) -> EventLedger {
        EventLedger::new(
            scenario
                .assertions
                .iter()
                .filter_map(assertion_filter)
                .collect(),
        )
    }

    #[test]
    fn complete_history_decides_event_assertions() {
        let scenario = example();
        let mut events = ledger(&scenario);
        assert!(!events.absorb(&page(serde_json::json!([ready(9)]), 0, 0, false, 9), 1));
        let results = observed(&scenario, events);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.outcome == Outcome::Pass));
    }

    #[test]
    fn dropped_events_never_pass() {
        let scenario = example();
        // Overflowed history, a pending (notification) loss and a sequence gap each poison the
        // window, even when the wanted event itself arrived.
        for (lost, pending, gap) in [(1, 0, false), (0, 3, false), (0, 0, true)] {
            let mut events = ledger(&scenario);
            events.absorb(
                &page(serde_json::json!([ready(9)]), lost, pending, gap, 9),
                1,
            );
            for result in observed(&scenario, events) {
                assert_eq!(result.outcome, Outcome::Inconclusive, "{result:?}");
            }
        }
        // A reply missing the loss counters is not proof of completeness either.
        let mut events = ledger(&scenario);
        events.absorb(&serde_json::json!({"events": [ready(9)], "latest": 9}), 1);
        assert!(observed(&scenario, events)
            .iter()
            .all(|result| result.outcome == Outcome::Inconclusive));
    }

    #[test]
    fn missing_observations_are_inconclusive() {
        let scenario = example();
        let results = evaluate(
            &scenario,
            &Observations {
                frames: 60,
                ..Observations::default()
            },
        );
        for result in &results {
            let expected = if matches!(result.kind.as_str(), "no_worker_crash" | "frame_count") {
                Outcome::Pass
            } else {
                Outcome::Inconclusive
            };
            assert_eq!(result.outcome, expected, "{result:?}");
        }
    }

    #[test]
    fn coverage_and_ranges_fail_when_violated() {
        let scenario = example();
        let mut seen = Observations {
            frames: 60,
            ..Observations::default()
        };
        seen.values.insert(2, 0xbad);
        seen.values.insert(3, 1);
        seen.hits.insert(7, 99);
        seen.hits.insert(8, 1);
        let results = evaluate(&scenario, &seen);
        for index in [2, 3, 7, 8] {
            assert_eq!(
                results[index].outcome,
                Outcome::Fail,
                "{:?}",
                results[index]
            );
        }
    }

    #[test]
    fn unfired_trigger_cannot_pass() {
        let filter = EventFilter {
            kind: "guest_log".into(),
            text: None,
        };
        assert_eq!(unfired_trigger(9, 1, &filter, false).outcome, Outcome::Fail);
        assert_eq!(
            unfired_trigger(9, 1, &filter, true).outcome,
            Outcome::Inconclusive
        );
    }

    #[test]
    fn validation_is_deterministic_and_strict() {
        let base = example();
        assert!(base.validate().is_ok());
        let reject = |edit: &dyn Fn(&mut Scenario)| {
            let mut scenario = base.clone();
            edit(&mut scenario);
            let first = scenario.validate().map_err(|error| error.to_string());
            assert_eq!(
                first,
                scenario.validate().map_err(|error| error.to_string())
            );
            assert!(first.is_err(), "{scenario:?}");
        };
        reject(&|s| s.assertions[2].width = Some(3));
        reject(&|s| s.assertions[2].at_frame = Some(61));
        reject(&|s| s.assertions[2].equals = None);
        reject(&|s| s.assertions[4].register = Some(17));
        reject(&|s| s.assertions[5].event = Some("pica".into()));
        reject(&|s| s.assertions[5].text_contains = Some("x".repeat(MAX_TEXT + 1)));
        reject(&|s| s.assertions[7].address = None);
        reject(&|s| s.assertions[0].kind = "marker".into());
        reject(&|s| s.assertions[0].kind = "fault_receipt".into());
        reject(&|s| {
            s.assertions[0].kind = "frame_hash".into();
            s.assertions[0].at_frame = Some(45);
            s.assertions[0].screen = Some("top".into());
            s.assertions[0].sha256 = Some("0".repeat(64));
        });
        reject(&|s| s.captures[1].when.as_mut().unwrap().frame = Some(10));
        reject(&|s| {
            let template = s.assertions[7].clone();
            s.assertions
                .extend(std::iter::repeat_n(template, MAX_COVERAGE));
        });
    }
}

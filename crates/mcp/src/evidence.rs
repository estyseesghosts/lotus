use crate::tool_arguments::EvidenceTraceSlice;
use base64::prelude::*;
use lotus_session::{investigation::SessionReference, supervisor::Supervisor};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
};

const MAX_BUNDLE_BYTES: u64 = 4 << 20;
const MAX_ARCHIVE_BYTES: u64 = 32 << 20;
const MAX_BUNDLES: usize = 64;

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub fn export(
    host: &Supervisor,
    id: &str,
    expected_revision: u64,
    screenshots: &[String],
    trace_slices: &[EvidenceTraceSlice],
    include_notes: bool,
) -> io::Result<Value> {
    if !host.allows_control() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "host control required",
        ));
    }
    if screenshots.len() > 2
        || screenshots.iter().collect::<BTreeSet<_>>().len() != screenshots.len()
    {
        return Err(invalid("select up to two distinct saved screenshots"));
    }
    if trace_slices.len() > 2
        || trace_slices
            .iter()
            .map(|slice| &slice.uri)
            .collect::<BTreeSet<_>>()
            .len()
            != trace_slices.len()
        || trace_slices
            .iter()
            .any(|slice| !(1..=128).contains(&slice.limit))
    {
        return Err(invalid(
            "select up to two distinct saved traces with limits of 1–128 events",
        ));
    }
    let view = host.investigation(id)?;
    if view.record.revision != expected_revision {
        return Err(invalid(
            "investigation revision mismatch; reload before exporting",
        ));
    }
    let details = view.record.details;
    let catalog = host.artifacts();
    let mut images = Vec::new();
    let mut observations = Vec::new();
    for uri in screenshots {
        let reference = details
            .artifacts
            .iter()
            .find(|reference| &reference.uri == uri)
            .ok_or_else(|| invalid("select screenshots from the saved artifact references"))?;
        let Some(artifact) = catalog.iter().find(|artifact| &artifact.uri == uri) else {
            observations.push(json!({"uri": uri, "confidence": "unknown", "status": "missing"}));
            continue;
        };
        if !matches!(artifact.screen.as_str(), "top" | "bottom")
            || artifact.mime_type != "application/octet-stream"
            || artifact.bytes > MAX_BUNDLE_BYTES as usize
        {
            return Err(invalid("only native screen captures can be included"));
        }
        if artifact.sha256 != reference.sha256 {
            observations
                .push(json!({"uri": uri, "confidence": "unknown", "status": "hash_mismatch"}));
            continue;
        }
        let mut native = match host.read_artifact(uri) {
            Ok(bytes) => bytes,
            Err(error) => {
                let status = match error.kind() {
                    io::ErrorKind::NotFound => "missing",
                    io::ErrorKind::InvalidData => "hash_mismatch",
                    _ => return Err(error),
                };
                observations.push(json!({"uri": uri, "confidence": "unknown", "status": status}));
                continue;
            }
        };
        let png = crate::screenshot::encode_png(&mut native, artifact.width, artifact.height)?;
        images.push(json!({
            "uri": uri, "confidence": "measured", "screen": artifact.screen,
            "width": artifact.width, "height": artifact.height, "mime_type": "image/png",
            "native_sha256": reference.sha256, "png_sha256": hash(&png),
            "data": BASE64_STANDARD.encode(png),
        }));
    }
    let traces = details.traces.iter().zip(view.trace_states).map(|(trace, state)| {
        json!({"uri": trace.uri, "sha256": trace.sha256, "status": state, "capture_included": false})
    }).collect::<Vec<_>>();
    let mut slices = Vec::new();
    for slice in trace_slices {
        let reference = details
            .traces
            .iter()
            .find(|reference| reference.uri == slice.uri)
            .ok_or_else(|| invalid("select trace slices from the saved trace references"))?;
        let observation = |status| {
            json!({
                "uri": slice.uri, "sha256": reference.sha256, "offset": slice.offset,
                "confidence": "unknown", "status": status,
            })
        };
        let Some(artifact) = catalog.iter().find(|artifact| artifact.uri == slice.uri) else {
            slices.push(observation("missing"));
            continue;
        };
        if artifact.mime_type != "application/x-citrace" || artifact.screen != "citrace" {
            return Err(invalid(
                "only native CiTrace streams can supply trace slices",
            ));
        }
        if artifact.sha256 != reference.sha256 {
            slices.push(observation("hash_mismatch"));
            continue;
        }
        let bytes = match host.read_artifact(&slice.uri) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                slices.push(observation("missing"));
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                slices.push(observation("hash_mismatch"));
                continue;
            }
            Err(error) => return Err(error),
        };
        slices.push(
            parse_citrace_slice(&bytes, slice, &reference.sha256)?
                .unwrap_or_else(|| observation("invalid_trace")),
        );
    }
    let sessions = details.sessions.iter().zip(view.session_states).map(|(reference, state)| {
        json!({"reference": reference, "status": state, "observation": observe_session(host, reference)})
    }).collect::<Vec<_>>();

    let notes = include_notes.then(|| {
        json!({
            "confidence": "unverified", "title": details.title, "purpose": details.purpose,
            "hypotheses": details.hypotheses, "next_steps": details.next_steps,
            "sources": details.sources, "breakpoints": details.breakpoints,
        })
    });
    let manifest = json!({
        "version": 1, "investigation_id": id, "revision": expected_revision, "notes": notes,
        "declared_build_hashes": details.build_hashes, "declared_content_hashes": details.content_hashes,
        "sessions": sessions, "traces": traces, "trace_slices": slices,
        "screenshot_observations": observations, "images": images,
        "unknown": [
                    "PICA draw history (capture a CiTrace to investigate draws)",
                    "Reproduction steps unless supplied in the opted-in notes"],
    });
    let bytes = serde_json::to_vec(&manifest)?;
    let bundle = hash(&bytes);
    publish(host, &bundle, &bytes)?;
    Ok(json!({"bundle": bundle, "uri": format!("lotus://evidence/{bundle}"), "bytes": bytes.len()}))
}

fn parse_citrace_slice(
    bytes: &[u8],
    slice: &EvidenceTraceSlice,
    sha256: &str,
) -> io::Result<Option<Value>> {
    // Packed little-endian CiTrace v1, src/core/tracer/citrace.h on the worker pin.
    const HEADER_BYTES: u32 = 100;
    const EVENT_BYTES: usize = 20;
    let word = |data: &[u8], i| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
    if bytes.len() < HEADER_BYTES as usize
        || &bytes[..8] != b"CiTr\x01\0\0\0"
        || word(bytes, 8) != HEADER_BYTES
    {
        return Ok(None);
    }
    let start = word(bytes, 92);
    let count = word(bytes, 96);
    if start < HEADER_BYTES
        || count > 262_144
        || u64::from(start) + u64::from(count) * EVENT_BYTES as u64 != bytes.len() as u64
    {
        return Ok(None);
    }
    if slice.offset >= count {
        return Err(invalid("trace slice offset is outside the recorded stream"));
    }
    let limit = slice.limit.min(count - slice.offset) as usize;
    let first = start as usize + slice.offset as usize * EVENT_BYTES;
    let mut events = Vec::with_capacity(limit);
    for event in bytes[first..first + limit * EVENT_BYTES].chunks_exact(EVENT_BYTES) {
        events.push(match word(event, 0) {
            0xE1 => json!({"kind": "frame_marker"}),
            0xE2 => json!({"kind": "memory_load", "bytes": word(event, 8),
                          "physical_address": word(event, 12)}),
            0xE3 => json!({"kind": "register_write", "physical_address": word(event, 4),
                          "value": word(event, 8)}),
            _ => return Ok(None),
        });
    }
    Ok(Some(json!({
        "uri": slice.uri, "sha256": sha256, "format": "citrace_v1",
        "confidence": "measured", "status": "included", "offset": slice.offset,
        "total_events": count, "events": events,
    })))
}

fn observe_session(host: &Supervisor, reference: &SessionReference) -> Value {
    let Ok(summary) = host.summary(&reference.session) else {
        return json!({"confidence": "unknown"});
    };
    let telemetry = summary.telemetry;
    if summary.info.created_unix_ms != reference.created_unix_ms
        || telemetry.epoch != reference.epoch
    {
        return json!({"confidence": "unknown"});
    }
    // The telemetry summary and GPU object can carry text from worker logs.
    let failure = telemetry.failure.map(|failure| {
        json!({
            "cause": failure["cause"], "exit_code": failure["exit_code"],
            "renderer": failure["renderer"],
        })
    });
    let stop_reason = telemetry
        .stop_reason
        .filter(|_| telemetry.state == lotus_session::SessionState::Paused);
    json!({
        "confidence": "measured", "state": telemetry.state, "frames": telemetry.frames,
        "renderer": telemetry.renderer, "failure": failure, "stop_reason": stop_reason,
        "identity": host.investigation_target_identity(reference),
    })
}

fn archive(host: &Supervisor) -> std::path::PathBuf {
    host.config().storage_root.join("evidence")
}

fn limit(host: &Supervisor) -> u64 {
    MAX_ARCHIVE_BYTES.min(host.config().artifacts.max_bytes)
}

fn publish(host: &Supervisor, id: &str, bytes: &[u8]) -> io::Result<()> {
    let length = bytes.len() as u64;
    if length > MAX_BUNDLE_BYTES || length > limit(host) {
        return Err(invalid("evidence bundle exceeds its byte limit"));
    }
    let directory = archive(host);
    fs::create_dir_all(&directory)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("archive.lock"))?;
    lock.lock()?;
    let path = directory.join(format!("{id}.json"));
    if path.try_exists()? {
        return verify(&path, id).map(|_| ());
    }
    let mut count = 0;
    let mut used = 0u64;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|ext| ext == "json") {
            count += 1;
            used = used.saturating_add(entry.metadata()?.len());
        }
    }
    if count >= MAX_BUNDLES || used.saturating_add(length) > limit(host) {
        return Err(invalid("evidence archive quota exceeded"));
    }
    let temporary = directory.join("bundle.tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)
}

fn verify(path: &Path, id: &str) -> io::Result<Value> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BUNDLE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES || hash(&bytes) != id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "evidence bundle hash mismatch",
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn read(host: &Supervisor, id: &str) -> io::Result<Value> {
    let id = id.strip_prefix("lotus://evidence/").unwrap_or(id);
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("expected a lowercase SHA-256 evidence bundle ID"));
    }
    verify(&archive(host).join(format!("{id}.json")), id)
}

pub fn summary(host: &Supervisor, id: &str) -> io::Result<Value> {
    let value = read(host, id)?;
    let mut report = value.clone();
    if let Some(images) = report["images"].as_array_mut() {
        for image in images {
            if let Some(metadata) = image.as_object_mut() {
                metadata.remove("data");
            }
        }
    }
    let markdown = format!(
        "# Lotus investigation evidence\n\nBundle `{id}`\n\n```json\n{}\n```\n",
        serde_json::to_string_pretty(&report)?.replace('`', "\\u0060"),
    );
    Ok(json!({"bundle": id, "manifest": report, "markdown": markdown}))
}

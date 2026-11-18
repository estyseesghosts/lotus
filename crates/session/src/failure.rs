//! Structured cause of a failed worker connection (lotus#47).
//!
//! When an exchange fails, times out or returns a malformed reply, [`crate::controlled_worker`]
//! kills and reaps the worker, then builds a [`FailureReport`] from what the host knows (last
//! request, exit code, transport error) and bounded tails of `worker.stderr.log` and
//! `log/azahar_log.txt`. Logs are read only after the process exited, at most [`TAIL_BYTES`]
//! each, on a blocking thread. The report is written to `<root>/failure.json`, which secondary
//! cleanup retains with the logs.
//!
//! [`classify`] recognizes a few known causes from log lines and keeps the original line;
//! anything else stays `unknown`. The one-line [`FailureReport::summary`] that reaches
//! `lotus_status`, MCP errors and the desktop has absolute paths redacted; the full report with
//! the tails stays local (session root and the desktop's copy action).
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde::Serialize;

/// Bytes read from the end of each log.
pub const TAIL_BYTES: u64 = 64 << 10;
/// Longest marker line kept in a summary.
const MARKER_CHARS: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// Vulkan allocation failed: `VK_ERROR_OUT_OF_DEVICE_MEMORY` (-2) or
    /// `VK_ERROR_OUT_OF_HOST_MEMORY` (-1), Vulkan specification `VkResult`.
    VulkanOutOfMemory,
    /// `VK_ERROR_DEVICE_LOST` (-4).
    VulkanDeviceLost,
    /// Another Vulkan error the worker reported as `worker_gpu_error` (cause `gpu_error`).
    VulkanError,
    /// The worker logged a critical error, panicked or exited nonzero on its own.
    Abort,
    /// The reply broke the control protocol.
    MalformedReply,
    /// No reply within the exchange deadline.
    Timeout,
    Unknown,
}

impl Cause {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::VulkanOutOfMemory => "vulkan_out_of_memory",
            Self::VulkanDeviceLost => "vulkan_device_lost",
            Self::VulkanError => "vulkan_error",
            Self::Abort => "abort",
            Self::MalformedReply => "malformed_reply",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
        }
    }

    /// Known renderer failure, as opposed to an unexplained disconnect.
    #[must_use]
    pub fn is_renderer(self) -> bool {
        matches!(
            self,
            Self::VulkanOutOfMemory | Self::VulkanDeviceLost | Self::VulkanError
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FailureReport {
    pub cause: Cause,
    /// The log line the cause was read from, verbatim (lossy UTF-8).
    pub marker: Option<String>,
    pub transport_error: String,
    pub exit_code: Option<i32>,
    pub pid: Option<u32>,
    /// Worker connection identity (not the Lotus session ID).
    pub connection: String,
    pub last_method: Option<String>,
    pub last_request_id: u64,
    /// The worker's `worker_gpu_error` report (lotus#46): `cause`, `vk_result`,
    /// `vk_result_name`, `object`, `gpu_memory` at the failure, `frame` when known.
    pub gpu: Option<serde_json::Value>,
    pub epoch: u64,
    pub host_time_unix_ms: u64,
    pub stderr_tail: String,
    pub log_tail: String,
}

impl FailureReport {
    /// One line without local paths: cause, marker, exit code and the failed request.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut text = format!("worker failed ({})", self.cause.name());
        if let Some(marker) = &self.marker {
            text.push_str(": ");
            text.push_str(&redact(marker));
        }
        if let Some(code) = self.exit_code {
            text.push_str(&format!("; exit code {code:#x}"));
        }
        if let Some(method) = &self.last_method {
            text.push_str(&format!("; during {method} #{}", self.last_request_id));
        }
        text
    }
}

/// Up to [`TAIL_BYTES`] from the end of `path`, starting at a line boundary. Missing files
/// are empty; invalid UTF-8 is replaced.
pub fn tail(path: &Path) -> io::Result<String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error),
    };
    let length = file.metadata()?.len();
    let start = length.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((length - start) as usize);
    file.take(TAIL_BYTES).read_to_end(&mut bytes)?;
    if start > 0 {
        match bytes.iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                bytes.drain(..=newline);
            }
            None => bytes.clear(),
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Prefix of the stderr line a CITnLO worker writes before exiting on a Vulkan failure
/// (`ctr_worker/control.h` `ReportGpuFailure`), followed by one JSON object.
pub const GPU_ERROR_PREFIX: &str = "worker_gpu_error: ";

/// The last `worker_gpu_error` report in `stderr`, if one parses.
#[must_use]
pub fn gpu_error(stderr: &str) -> Option<serde_json::Value> {
    stderr.lines().rev().find_map(|line| {
        let json = line.trim().strip_prefix(GPU_ERROR_PREFIX)?;
        serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .filter(serde_json::Value::is_object)
    })
}

/// Cause and marker for a `worker_gpu_error` report: the failed object and Vulkan result.
fn classify_gpu(report: &serde_json::Value) -> (Cause, String) {
    let cause = match report["cause"].as_str() {
        Some("gpu_out_of_memory") => Cause::VulkanOutOfMemory,
        Some("gpu_device_lost") => Cause::VulkanDeviceLost,
        _ => Cause::VulkanError,
    };
    let marker = format!(
        "{} ({}) creating {}",
        report["vk_result_name"].as_str().unwrap_or("?"),
        report["vk_result"].as_i64().unwrap_or(0),
        report["object"].as_str().unwrap_or("?"),
    );
    (cause, marker)
}

/// Cause and marker line from the log tails, the exit code and the transport error. A
/// `worker_gpu_error` report on stderr takes precedence over log lines.
#[must_use]
pub fn classify(
    log: &str,
    stderr: &str,
    exit_code: Option<i32>,
    transport: io::ErrorKind,
) -> (Cause, Option<String>) {
    if let Some(report) = gpu_error(stderr) {
        let (cause, line) = classify_gpu(&report);
        return (cause, Some(marker(&line)));
    }
    let lines = || log.lines().chain(stderr.lines());
    // Azahar's Vulkan texture runtime logs `Failed allocating image with error -2`
    // (vk_texture_runtime.cpp, LOG_CRITICAL before UNREACHABLE).
    let vulkan = |line: &str, names: &[&str], codes: &[&str]| {
        names.iter().any(|name| line.contains(name))
            || (line.contains("Vulkan")
                && codes
                    .iter()
                    .any(|code| line.contains(&format!("error {code}"))))
    };
    for line in lines() {
        if vulkan(
            line,
            &[
                "VK_ERROR_OUT_OF_DEVICE_MEMORY",
                "VK_ERROR_OUT_OF_HOST_MEMORY",
            ],
            &["-2", "-1"],
        ) {
            return (Cause::VulkanOutOfMemory, Some(marker(line)));
        }
        if vulkan(line, &["VK_ERROR_DEVICE_LOST"], &["-4"]) {
            return (Cause::VulkanDeviceLost, Some(marker(line)));
        }
    }
    let critical = lines().find(|line| line.contains("<Critical>") || line.contains("panicked at"));
    if let Some(line) = critical {
        return (Cause::Abort, Some(marker(line)));
    }
    match transport {
        io::ErrorKind::TimedOut => (Cause::Timeout, None),
        io::ErrorKind::InvalidData => (Cause::MalformedReply, None),
        _ if exit_code.is_some_and(|code| code != 0) => (Cause::Abort, None),
        _ => (Cause::Unknown, None),
    }
}

fn marker(line: &str) -> String {
    line.trim().chars().take(MARKER_CHARS).collect()
}

/// Replace absolute Windows (`C:\…`, `C:/…`) and POSIX (`/home/…`) path tokens with `<path>`.
#[must_use]
pub fn redact(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let bytes = word.as_bytes();
            let windows = bytes.len() > 2
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/');
            let posix = word.starts_with('/') && word[1..].contains('/');
            if windows || posix || word.starts_with("\\\\") {
                "<path>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two final lines of runs/div29-both-vk-oom/r0/w0/log/azahar_log.txt.
    const OOM: &str = "[ 305.918224] Render.Vulkan <Critical> video_core/renderer_vulkan/vk_texture_runtime.cpp:Create:224: Failed allocating image with error -2\n[ 305.918251] Debug <Critical> video_core/renderer_vulkan/vk_texture_runtime.cpp:operator():225: Unreachable code!\n";

    #[test]
    fn the_recorded_vulkan_oom_is_classified_with_its_line() {
        let (cause, marker) = classify(OOM, "", Some(3), io::ErrorKind::BrokenPipe);
        assert_eq!(cause, Cause::VulkanOutOfMemory);
        assert!(marker
            .unwrap()
            .contains("Failed allocating image with error -2"));
    }

    /// Shape of the line `ReportGpuFailure` wrote in the injected sweep (lotus#46).
    const GPU: &str = "worker_gpu_error: {\"cause\":\"gpu_out_of_memory\",\"frame\":312,\"gpu_memory\":{\"allocation_failures\":1,\"budget_source\":\"VK_EXT_memory_budget\",\"heaps\":[]},\"object\":\"image\",\"vk_result\":-2,\"vk_result_name\":\"VK_ERROR_OUT_OF_DEVICE_MEMORY\"}\n";

    #[test]
    fn the_worker_gpu_report_is_parsed_and_wins_over_log_lines() {
        let critical = "[1.0] Render.Vulkan <Critical> something\n";
        let (cause, marker) = classify(critical, GPU, Some(4), io::ErrorKind::BrokenPipe);
        assert_eq!(cause, Cause::VulkanOutOfMemory);
        assert_eq!(
            marker.as_deref(),
            Some("VK_ERROR_OUT_OF_DEVICE_MEMORY (-2) creating image")
        );
        let report = gpu_error(GPU).unwrap();
        assert_eq!(report["frame"], 312);
        assert_eq!(
            report["gpu_memory"]["budget_source"],
            "VK_EXT_memory_budget"
        );
        let lost = GPU.replace("gpu_out_of_memory", "gpu_device_lost");
        assert_eq!(
            classify("", &lost, Some(4), io::ErrorKind::BrokenPipe).0,
            Cause::VulkanDeviceLost
        );
        let other = GPU.replace("gpu_out_of_memory", "gpu_error");
        assert!(classify("", &other, Some(4), io::ErrorKind::BrokenPipe)
            .0
            .is_renderer());
        // A truncated line is ignored, not misparsed.
        assert!(gpu_error("worker_gpu_error: {\"cause\":").is_none());
    }

    #[test]
    fn other_causes_and_unknown() {
        let critical = "[1.0] Core <Critical> core/core.cpp:Run:1: something broke\n";
        assert_eq!(
            classify(critical, "", None, io::ErrorKind::BrokenPipe).0,
            Cause::Abort
        );
        assert_eq!(
            classify(
                "",
                "thread 'main' panicked at src/x.rs",
                None,
                io::ErrorKind::BrokenPipe
            )
            .0,
            Cause::Abort
        );
        assert_eq!(
            classify("", "", None, io::ErrorKind::TimedOut).0,
            Cause::Timeout
        );
        assert_eq!(
            classify("", "", None, io::ErrorKind::InvalidData).0,
            Cause::MalformedReply
        );
        assert_eq!(
            classify("", "", Some(5), io::ErrorKind::BrokenPipe).0,
            Cause::Abort
        );
        assert_eq!(
            classify("plain info\n", "", Some(0), io::ErrorKind::BrokenPipe),
            (Cause::Unknown, None)
        );
        // A log line merely containing "-2" without Vulkan context is not an OOM.
        assert_eq!(
            classify(
                "[1] Service <Error> result -2\n",
                "",
                None,
                io::ErrorKind::BrokenPipe
            )
            .0,
            Cause::Unknown
        );
    }

    #[test]
    fn tails_are_bounded_lossy_and_line_aligned() {
        let base = std::env::temp_dir().join(format!(
            "lotus-tail-{}",
            lotus_protocol::envelope::SessionUuid::generate()
        ));
        std::fs::create_dir_all(&base).expect("dir");
        assert_eq!(tail(&base.join("missing.log")).expect("missing"), "");
        // 100 MiB of filler, invalid UTF-8, then the marker.
        let big = base.join("big.log");
        {
            use std::io::Write;
            let mut file = File::create(&big).expect("create");
            let line = [b'x'; 1023];
            for _ in 0..(100 << 10) {
                file.write_all(&line).expect("write");
                file.write_all(b"\n").expect("write");
            }
            file.write_all(b"\xff\xfe bad\n").expect("write");
            file.write_all(OOM.as_bytes()).expect("write");
        }
        let text = tail(&big).expect("tail");
        assert!(text.len() as u64 <= TAIL_BYTES + 64, "{}", text.len());
        assert!(text.starts_with('x') && text.ends_with("Unreachable code!\n"));
        assert!(text.contains('\u{fffd}'));
        // A truncated last line (no newline) is kept.
        let short = base.join("short.log");
        std::fs::write(&short, b"one\ntwo").expect("write");
        assert_eq!(tail(&short).expect("tail"), "one\ntwo");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn summaries_redact_paths() {
        let report = FailureReport {
            cause: Cause::Abort,
            marker: Some(
                "<Critical> could not open C:\\Users\\me\\game.3dsx or /home/me/x.bin".into(),
            ),
            transport_error: String::new(),
            exit_code: Some(3),
            pid: None,
            connection: String::new(),
            last_method: Some("Status".into()),
            last_request_id: 7,
            gpu: None,
            epoch: 1,
            host_time_unix_ms: 0,
            stderr_tail: String::new(),
            log_tail: String::new(),
        };
        let summary = report.summary();
        assert!(
            !summary.contains("Users") && !summary.contains("/home"),
            "{summary}"
        );
        assert!(summary.contains("exit code 0x3") && summary.contains("Status #7"));
    }
}

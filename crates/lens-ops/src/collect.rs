//! Explicit, bounded adapters for external log sources. No shell, privilege
//! escalation, service mutation, or discovery of unrelated containers.
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Report, Severity};
use lens_core::{BundleArtifact, LensError, Result};

#[derive(Debug, Clone, Copy)]
pub enum Adapter {
    Journal,
    Docker,
    Podman,
}

pub struct CollectRequest {
    pub adapter: Adapter,
    pub target: String,
    pub user: bool,
    pub since: Option<String>,
    pub lines: usize,
    pub timeout: Duration,
    pub output: PathBuf,
    pub force: bool,
    pub redact: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct CollectionResult {
    pub schema: &'static str,
    pub argv: Vec<String>,
    pub target: String,
    pub collected_at: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub output: PathBuf,
}

fn arguments(req: &CollectRequest) -> Result<Vec<String>> {
    if req.target.is_empty() || req.target.starts_with('-') || req.target.contains('\0') {
        return Err(LensError::Usage {
            message: "select a nonempty unit/container name that does not start with '-'".into(),
        });
    }
    if req.lines == 0
        || req.lines > 1_000_000
        || req.timeout.is_zero()
        || req.timeout > Duration::from_secs(300)
    {
        return Err(LensError::Usage {
            message: "lines must be 1..1000000 and timeout must be 1..300 seconds".into(),
        });
    }
    let mut args = match req.adapter {
        Adapter::Journal => {
            let mut args = vec![
                "journalctl".into(),
                "--no-pager".into(),
                "--output=short-iso-precise".into(),
                format!("--lines={}", req.lines),
                format!("--unit={}", req.target),
            ];
            if req.user {
                args.push("--user".into());
            }
            args
        }
        Adapter::Docker | Adapter::Podman => {
            if req.user {
                return Err(LensError::Usage {
                    message: "--user is a journal manager selector only".into(),
                });
            }
            vec![
                if matches!(req.adapter, Adapter::Docker) {
                    "docker"
                } else {
                    "podman"
                }
                .into(),
                "logs".into(),
                "--timestamps".into(),
                format!("--tail={}", req.lines),
            ]
        }
    };
    if let Some(since) = &req.since {
        args.push(format!("--since={since}"));
    }
    if !matches!(req.adapter, Adapter::Journal) {
        args.push(req.target.clone());
    }
    Ok(args)
}

struct Capture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: Option<i32>,
    timed_out: bool,
    truncated: bool,
}

fn nonblocking(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// Read at most one bounded batch per poll so a perpetually writable stdout
// cannot starve stderr, timeout or exit handling.
fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>, cap: usize) -> std::io::Result<(bool, bool)> {
    let mut buf = [0u8; 8192];
    for _ in 0..16 {
        match reader.read(&mut buf) {
            Ok(0) => return Ok((true, false)),
            Ok(n) => {
                let keep = n.min(cap.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buf[..keep]);
                if keep < n {
                    return Ok((false, true));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok((false, false)),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok((false, false))
}

fn capture(argv: &[String], timeout: Duration, cap: usize) -> std::io::Result<Capture> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("SYSTEMD_PAGER", "")
        .env("SYSTEMD_COLORS", "0")
        .process_group(0)
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let terminate = |child: &mut std::process::Child| {
        // The group was created for this invocation; never signal the caller's group.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    };
    if let Err(error) =
        nonblocking(stdout.as_raw_fd()).and_then(|_| nonblocking(stderr.as_raw_fd()))
    {
        terminate(&mut child);
        return Err(error);
    }
    let start = Instant::now();
    let mut out = Capture {
        stdout: Vec::new(),
        stderr: Vec::new(),
        exit_code: None,
        timed_out: false,
        truncated: false,
    };
    loop {
        let polled = (|| -> std::io::Result<_> {
            let (out_done, out_full) = drain(&mut stdout, &mut out.stdout, cap)?;
            let (err_done, err_full) = drain(&mut stderr, &mut out.stderr, cap)?;
            let status = child.try_wait()?;
            Ok((out_done && err_done, out_full || err_full, status))
        })();
        let (streams_done, truncated, status) = match polled {
            Ok(value) => value,
            Err(error) => {
                terminate(&mut child);
                return Err(error);
            }
        };
        if let Some(status) = status {
            out.exit_code = status.code();
        }
        out.truncated |= truncated;
        if streams_done && status.is_some() {
            break;
        }
        out.timed_out = start.elapsed() >= timeout;
        if out.truncated || out.timed_out {
            terminate(&mut child);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(out)
}

/// Capture a selected source into a verified .lens bundle. stdout/stderr remain
/// separate: container stderr may itself contain application logs.
pub fn collect(req: &CollectRequest) -> Result<Report<CollectionResult>> {
    if !cfg!(target_os = "linux") {
        return Err(LensError::Usage {
            message: "external collection adapters are supported on Linux only".into(),
        });
    }
    let argv = arguments(req)?;
    if req.output.exists() && !req.force {
        return Err(LensError::Usage {
            message: format!("output {:?} exists; use --force", req.output),
        });
    }
    let capture = capture(&argv, req.timeout, 8 * 1024 * 1024).map_err(|source| LensError::Io {
        path: Path::new(&argv[0]).to_path_buf(),
        source,
    })?;
    let mut report = Report::new(CollectionResult {
        schema: "lens.collection/v1",
        argv,
        target: req.target.clone(),
        collected_at: lens_core::time::utc_now_iso(),
        exit_code: capture.exit_code,
        timed_out: capture.timed_out,
        truncated: capture.truncated,
        stdout_bytes: capture.stdout.len(),
        stderr_bytes: capture.stderr.len(),
        output: req.output.clone(),
    });
    report.complete = capture.exit_code == Some(0) && !capture.timed_out && !capture.truncated;
    if !report.complete {
        report.push(
            Severity::Warn,
            "collect.incomplete",
            "Requested source did not finish successfully; captured bytes are partial evidence",
        );
        report.diagnostics.push(format!(
            "source exit={:?}, timeout={}, byte_limit={}",
            capture.exit_code, capture.timed_out, capture.truncated
        ));
    }
    let raw = |name: &str, data: Vec<u8>| -> Result<BundleArtifact> {
        Ok(BundleArtifact {
            name: name.into(),
            data: if req.redact {
                lens_core::try_redact_bytes(&data)?
            } else {
                data
            },
        })
    };
    let artifacts = vec![
        BundleArtifact::json_with_redaction("reports/collection.json", &report.data, req.redact)?,
        raw("logs/stdout.log", capture.stdout)?,
        raw("logs/stderr.log", capture.stderr)?,
    ];
    lens_core::create_bundle_archive_with_redaction(
        &req.output,
        env!("CARGO_PKG_VERSION"),
        &artifacts,
        report.diagnostics.clone(),
        req.force,
        req.redact,
    )?;
    report.diagnostics.push("Scope: explicitly selected unit/container, requested tail/time bounds, current account permissions and runtime endpoint; stdout/stderr are retained separately.".into());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_keeps_exit_and_streams_separate() {
        let argv = ["/bin/sh", "-c", "printf out; printf err >&2; exit 7"].map(str::to_string);
        let c = capture(&argv, Duration::from_secs(2), 128).unwrap();
        assert_eq!(c.exit_code, Some(7));
        assert_eq!(c.stdout, b"out");
        assert_eq!(c.stderr, b"err");
        assert!(!c.timed_out);
        assert!(!c.truncated);
    }
    #[test]
    fn capture_timeout_and_byte_limit_are_bounded() {
        let argv = ["/bin/sh", "-c", "sleep 3"].map(str::to_string);
        let start = Instant::now();
        assert!(
            capture(&argv, Duration::from_millis(30), 128)
                .unwrap()
                .timed_out
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        let argv = ["/bin/sh", "-c", "while :; do printf abcdefgh; done"].map(str::to_string);
        let c = capture(&argv, Duration::from_secs(2), 32).unwrap();
        assert!(c.truncated);
        assert_eq!(c.stdout.len(), 32);
    }
}

use std::path::Path;

use lens_core::Result;
use lens_net::{diff_net_reports, inspect_network, NetDiffReport, NetReport};

use crate::{json, Report, Severity};

/// Inspect sockets via procfs. `no_unix` drops unix-domain sockets.
/// Orphaned sockets are a Warn finding — they hint at evidence the
/// process table could not attribute.
pub fn inspect(proc_dir: Option<&Path>, no_unix: bool) -> Result<Report<NetReport>> {
    let mut net = inspect_network(proc_dir)?;
    if no_unix {
        net.sockets.retain(|s| {
            !matches!(
                s.kind,
                lens_net::SocketKind::UnixStream | lens_net::SocketKind::UnixDgram
            )
        });
    }
    let mut report = Report::new(net);
    if report.data.summary.orphan_sockets > 0 {
        report.push(
            Severity::Warn,
            "net.orphan",
            format!(
                "{} orphaned socket(s) (no live process holds the inode)",
                report.data.summary.orphan_sockets
            ),
        );
    }
    Ok(report)
}

/// Diff two net snapshot JSONs (schema-checked). New listeners are Fail
/// findings — a port nobody opened before is the highest-value delta.
pub fn diff(baseline: &Path, candidate: &Path) -> Result<Report<NetDiffReport>> {
    let b: NetReport =
        json::read_json_schema(baseline, "schema_version", lens_net::REPORT_SCHEMA_V1)?;
    let c: NetReport =
        json::read_json_schema(candidate, "schema_version", lens_net::REPORT_SCHEMA_V1)?;
    let diff = diff_net_reports(&b, &c);
    let (new_listeners, closed_listeners, new_conns, closed_conns, tw) = (
        diff.new_listeners.clone(),
        diff.closed_listeners.len(),
        diff.new_connections.len(),
        diff.closed_connections.len(),
        diff.time_wait_delta,
    );
    let mut report = Report::new(diff);
    for l in &new_listeners {
        report.push(
            Severity::Fail,
            "net.new_listener",
            format!("{}:{}", l.local_address, l.local_port),
        );
    }
    if closed_listeners > 0 {
        report.push(
            Severity::Info,
            "net.closed_listeners",
            format!("{closed_listeners} listener(s) closed"),
        );
    }
    if new_conns > 0 || closed_conns > 0 {
        report.push(
            Severity::Info,
            "net.connection_delta",
            format!("+{new_conns} -{closed_conns} connection(s), TIME_WAIT {tw:+}"),
        );
    }
    Ok(report)
}

use std::fs;
use std::path::Path;

use lens_core::{LensError, Result};

/// Parse a JSON file with the path in the error, so a malformed input
/// reports *which* file failed rather than a bare serde line/column.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let content = fs::read_to_string(path).map_err(|e| LensError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    serde_json::from_str(&content).map_err(|e| LensError::InvalidInput {
        message: format!("{}: {}", path.display(), e),
    })
}

/// Read a snapshot JSON and refuse it when the `key` field does not equal
/// `expected` — a `net` report fed to `env diff` must be a usage error
/// (exit 2), not a misleadingly-empty diff of the wrong schema.
pub fn read_json_schema<T: serde::de::DeserializeOwned>(
    path: &Path,
    key: &str,
    expected: &str,
) -> Result<T> {
    let content = fs::read_to_string(path).map_err(|e| LensError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| LensError::InvalidInput {
            message: format!("{}: {}", path.display(), e),
        })?;
    // Producers use either `schema` (env/sys) or `schema_version` (net);
    // check both so a foreign report names its real schema in the error
    // instead of reporting "<missing>".
    let actual = value
        .get(key)
        .or_else(|| value.get("schema"))
        .or_else(|| value.get("schema_version"))
        .and_then(|s| s.as_str());
    if actual != Some(expected) {
        return Err(LensError::InvalidInput {
            message: format!(
                "{}: expected schema {}, got {}",
                path.display(),
                expected,
                actual.unwrap_or("<missing>")
            ),
        });
    }
    serde_json::from_value(value).map_err(|e| LensError::InvalidInput {
        message: format!("{}: {}", path.display(), e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_json_schema_rejects_wrong_schema() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.json");
        fs::write(&file, r#"{"schema_version":"lens.net/v1"}"#).unwrap();

        let res: Result<serde_json::Value> =
            read_json_schema(&file, "schema", "envlens.snapshot/v1");
        let err = res.unwrap_err().to_string();
        assert!(err.contains("expected schema envlens.snapshot/v1"), "{err}");
        assert!(err.contains("report.json"), "{err}");
    }

    #[test]
    fn read_json_schema_accepts_matching_schema() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("snap.json");
        fs::write(&file, r#"{"schema":"servicelens.snapshot/v1","extra":1}"#).unwrap();
        let v: serde_json::Value =
            read_json_schema(&file, "schema", "servicelens.snapshot/v1").unwrap();
        assert_eq!(v["extra"], 1);
    }

    /// A minimal valid `lens.net/v1` report — foreign schema for the
    /// env/sys diff readers, matching schema for net.
    fn write_net_report(path: &Path) {
        fs::write(
            path,
            r#"{"schema_version":"lens.net/v1","summary":{"total_sockets":0,"listening_ports":0,"established_connections":0,"time_wait_sockets":0,"orphan_sockets":0,"unix_domain_sockets":0},"sockets":[],"listening":[]}"#,
        )
        .unwrap();
    }

    #[test]
    fn env_diff_rejects_net_report_schema() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.json");
        let b = dir.path().join("b.json");
        write_net_report(&a);
        write_net_report(&b);
        let err = crate::env::diff(&a, &b).unwrap_err().to_string();
        assert!(err.contains("expected schema"), "{err}");
        assert!(err.contains("lens.net/v1"), "{err}");
    }

    #[test]
    fn sys_diff_rejects_net_report_schema() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.json");
        let b = dir.path().join("b.json");
        write_net_report(&a);
        write_net_report(&b);
        let err = crate::sys::diff(&a, &b).unwrap_err().to_string();
        assert!(err.contains("expected schema"), "{err}");
    }

    #[test]
    fn net_diff_rejects_sys_snapshot_schema() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.json");
        let b = dir.path().join("b.json");
        for p in [&a, &b] {
            fs::write(
                p,
                r#"{"schema":"servicelens.snapshot/v1","version":"0.0.0","semantics":"test","units":{},"cycles":[],"diagnostics":[]}"#,
            )
            .unwrap();
        }
        let err = crate::net::diff(&a, &b).unwrap_err().to_string();
        assert!(err.contains("expected schema lens.net/v1"), "{err}");
    }
}

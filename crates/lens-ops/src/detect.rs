//! Input auto-detection for `lens inspect` / `lens diff`: decide which
//! domain an input belongs to from the filesystem entry itself —
//! extension, magic bytes, and at most the first 64 KiB of content.

use std::fs;
use std::io::Read;
use std::path::Path;

use lens_core::{LensError, Result};

/// What a path turned out to be. `lens inspect` maps each to a domain
/// command; `lens diff` pairs compatible kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Elf,
    Junit,
    Strace,
    CompileDb,
    Venv,
    SystemdDir,
    SystemdUnit,
    SysSnapshot,
    EnvSnapshot,
    NetReport,
    Bundle,
    Log,
    Directory,
}

/// Sniffing reads at most this much of any file.
const SNIFF_BYTES: usize = 64 * 1024;

const UNIT_EXTENSIONS: &[&str] = &[
    "service",
    "socket",
    "target",
    "timer",
    "mount",
    "automount",
    "swap",
    "path",
    "slice",
    "scope",
];

fn read_head(path: &Path, cap: usize) -> Result<Vec<u8>> {
    let f = lens_core::read_policy::open(path).map_err(|e| LensError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut buf = Vec::new();
    f.take(cap as u64)
        .read_to_end(&mut buf)
        .map_err(|e| LensError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    Ok(buf)
}

/// Skip a BOM, `<?xml …?>` prologs, comments, and whitespace, then check
/// for the JUnit root element. `<testsuite` covers both `<testsuite>`
/// and `<testsuites>`.
fn looks_like_junit(head: &[u8]) -> bool {
    let mut s = head;
    if s.starts_with(&[0xEF, 0xBB, 0xBF]) {
        s = &s[3..];
    }
    loop {
        while s.first().is_some_and(|b| b.is_ascii_whitespace()) {
            s = &s[1..];
        }
        if let Some(rest) = s.strip_prefix(b"<?xml") {
            match rest.windows(2).position(|w| w == b"?>") {
                Some(i) => s = &rest[i + 2..],
                None => return false,
            }
            continue;
        }
        if let Some(rest) = s.strip_prefix(b"<!--") {
            match rest.windows(3).position(|w| w == b"-->") {
                Some(i) => s = &rest[i + 3..],
                None => return false,
            }
            continue;
        }
        break;
    }
    s.starts_with(b"<testsuite")
}

pub(crate) fn has_unit_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| UNIT_EXTENSIONS.contains(&e))
}

#[derive(serde::Deserialize)]
struct SchemaProbe {
    schema: Option<String>,
    schema_version: Option<String>,
}

impl SchemaProbe {
    fn schema_str(&self) -> Option<&str> {
        self.schema.as_deref().or(self.schema_version.as_deref())
    }
}

/// Stream-parse just the first element of a `[`-starting file and check
/// it for the compile_commands shape (`directory` + `file` keys).
/// Deliberately not bounded by the 64 KiB sniff — a renamed compile db
/// must still be recognized no matter where its first entry ends.
fn first_array_element_has_compile_shape(path: &Path) -> bool {
    use serde::de::{IgnoredAny, SeqAccess, Visitor};
    use serde::Deserializer as _;
    use std::fmt;

    struct FirstElement;
    impl<'de> Visitor<'de> for FirstElement {
        type Value = Option<serde_json::Value>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a JSON array")
        }

        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let first = seq.next_element::<serde_json::Value>()?;
            // Drain the rest cheaply so the deserializer sees the `]`.
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(first)
        }
    }

    let Ok(f) = lens_core::read_policy::open(path) else {
        return false;
    };
    let mut de = serde_json::Deserializer::from_reader(std::io::BufReader::new(f));
    let Ok(first) = de.deserialize_seq(FirstElement) else {
        return false;
    };
    first
        .as_ref()
        .and_then(|v| v.as_object())
        .is_some_and(|o| o.contains_key("directory") && o.contains_key("file"))
}

/// Classify a path. First match wins; an unreadable/nonexistent path is
/// an `Io` error (exit 2). Files that merely look structured but carry
/// no recognized schema classify as logs — guessing a domain would
/// silently produce a meaningless report.
pub fn detect(path: &Path) -> Result<InputKind> {
    let sym_meta = fs::symlink_metadata(path).map_err(|e| LensError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let is_dir = if sym_meta.file_type().is_symlink() {
        if has_unit_extension(path) {
            lens_core::read_policy::check_boundary(path).map_err(|e| LensError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            return Ok(InputKind::SystemdUnit);
        }
        lens_core::read_policy::metadata(path)
            .map(|m| m.is_dir())
            .unwrap_or(false)
    } else {
        lens_core::read_policy::check_boundary(path).map_err(|e| LensError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        sym_meta.is_dir()
    };

    if is_dir {
        if path.join("pyvenv.cfg").is_file() {
            return Ok(InputKind::Venv);
        }
        if let Ok(rd) = lens_core::read_policy::read_dir(path) {
            let mut has_unit = false;
            let mut has_junit = false;
            for entry in rd.flatten() {
                let p = entry.path();
                if has_unit_extension(&p) {
                    has_unit = true;
                    break;
                }
                if !has_junit
                    && p.extension().is_some_and(|e| e == "xml")
                    && read_head(&p, 8192)
                        .map(|h| looks_like_junit(&h))
                        .unwrap_or(false)
                {
                    has_junit = true;
                }
            }
            if has_unit {
                return Ok(InputKind::SystemdDir);
            }
            if has_junit {
                return Ok(InputKind::Junit);
            }
        }
        return Ok(InputKind::Directory);
    }

    let head = read_head(path, SNIFF_BYTES)?;
    if head.starts_with(b"\x7fELF") {
        return Ok(InputKind::Elf);
    }
    if path.extension().is_some_and(|e| e == "lens")
        || (head.starts_with(&[0x1f, 0x8b]) && lens_core::inspect_bundle(path).is_ok())
    {
        return Ok(InputKind::Bundle);
    }
    if has_unit_extension(path) {
        return Ok(InputKind::SystemdUnit);
    }
    if looks_like_junit(&head) {
        return Ok(InputKind::Junit);
    }

    let first_non_ws = head.iter().find(|b| !b.is_ascii_whitespace());
    match first_non_ws {
        Some(b'[') => {
            // JSON array: compile_commands by name, or a first element
            // carrying `directory` + `file` keys — stream-parsed so a
            // renamed db larger than the 64 KiB sniff still detects.
            if path
                .file_name()
                .is_some_and(|n| n == "compile_commands.json")
                || first_array_element_has_compile_shape(path)
            {
                return Ok(InputKind::CompileDb);
            }
            Ok(InputKind::Log)
        }
        Some(b'{') => {
            // A snapshot/report: stream-parse just the schema fields without
            // materializing a full DOM in memory. Anything else that merely
            // starts with `{` — JSONL streams, concatenated objects, foreign
            // JSON — is just a log.
            let f = lens_core::read_policy::open(path).map_err(|e| LensError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            let Ok(probe) = serde_json::from_reader::<_, SchemaProbe>(std::io::BufReader::new(f))
            else {
                return Ok(InputKind::Log);
            };
            match probe.schema_str() {
                Some(lens_env::SNAPSHOT_SCHEMA_V1) => Ok(InputKind::EnvSnapshot),
                Some(lens_sys::SNAPSHOT_SCHEMA_V1) => Ok(InputKind::SysSnapshot),
                Some(lens_net::REPORT_SCHEMA_V1) => Ok(InputKind::NetReport),
                _ => Ok(InputKind::Log),
            }
        }
        _ => {
            // Plain text: strace validation is fail-closed, so it doubles
            // as the recognizer — markdown never parses as strace.
            let text = String::from_utf8_lossy(&head);
            let head_lines: String = text.lines().take(200).collect::<Vec<_>>().join("\n");
            if lens_trace::analyze_strace_text(&head_lines).is_ok() {
                return Ok(InputKind::Strace);
            }
            Ok(InputKind::Log)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn detects_elf() {
        let dir = tempdir().unwrap();
        let mut bytes = b"\x7fELF".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        let p = write(dir.path(), "bin", &bytes);
        assert_eq!(detect(&p).unwrap(), InputKind::Elf);
    }

    #[test]
    fn detects_junit_with_prolog() {
        let dir = tempdir().unwrap();
        let p = write(
            dir.path(),
            "r.xml",
            br#"<?xml version="1.0"?>
<!-- generated -->
<testsuite tests="1"><testcase name="a"/></testsuite>"#,
        );
        assert_eq!(detect(&p).unwrap(), InputKind::Junit);
    }

    #[test]
    fn detects_strace() {
        let dir = tempdir().unwrap();
        let p = write(
            dir.path(),
            "t.log",
            b"1 openat(AT_FDCWD, \"/a\", O_RDONLY) = 3\n1 close(3) = 0\n",
        );
        assert_eq!(detect(&p).unwrap(), InputKind::Strace);
    }

    #[test]
    fn detects_compile_db_by_name_and_by_shape() {
        let dir = tempdir().unwrap();
        let by_name = write(
            dir.path(),
            "compile_commands.json",
            br#"[{"directory":"/tmp","command":"cc -c a.c","file":"a.c"}]"#,
        );
        assert_eq!(detect(&by_name).unwrap(), InputKind::CompileDb);
        let by_shape = write(
            dir.path(),
            "ccdb.json",
            br#"[{"directory":"/tmp","command":"cc -c a.c","file":"a.c"}]"#,
        );
        assert_eq!(detect(&by_shape).unwrap(), InputKind::CompileDb);
        let just_array = write(dir.path(), "data.json", br#"[{"x":1}]"#);
        assert_eq!(detect(&just_array).unwrap(), InputKind::Log);
    }

    #[test]
    fn detects_venv_and_systemd_and_plain_dirs() {
        let dir = tempdir().unwrap();
        let venv = dir.path().join("venv");
        fs::create_dir_all(&venv).unwrap();
        fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        assert_eq!(detect(&venv).unwrap(), InputKind::Venv);

        let units = dir.path().join("units");
        fs::create_dir_all(&units).unwrap();
        fs::write(units.join("a.service"), "[Service]\n").unwrap();
        assert_eq!(detect(&units).unwrap(), InputKind::SystemdDir);

        let plain = dir.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        fs::write(plain.join("readme.txt"), "hi\n").unwrap();
        assert_eq!(detect(&plain).unwrap(), InputKind::Directory);
    }

    #[test]
    fn detects_unit_file() {
        let dir = tempdir().unwrap();
        let p = write(dir.path(), "a.timer", b"[Timer]\nOnBootSec=1m\n");
        assert_eq!(detect(&p).unwrap(), InputKind::SystemdUnit);
    }

    #[test]
    fn detects_snapshot_schemas() {
        let dir = tempdir().unwrap();
        let env = write(
            dir.path(),
            "env.json",
            br#"{"schema":"envlens.snapshot/v1","version":"x","venv":{}}"#,
        );
        assert_eq!(detect(&env).unwrap(), InputKind::EnvSnapshot);
        let sys = write(
            dir.path(),
            "sys.json",
            br#"{"schema":"servicelens.snapshot/v1","version":"x","semantics":"s","units":{},"cycles":[],"diagnostics":[]}"#,
        );
        assert_eq!(detect(&sys).unwrap(), InputKind::SysSnapshot);
        let net = write(
            dir.path(),
            "net.json",
            br#"{"schema_version":"lens.net/v1","summary":{},"sockets":[],"listening":[]}"#,
        );
        assert_eq!(detect(&net).unwrap(), InputKind::NetReport);
    }

    #[test]
    fn unknown_schema_json_is_a_log() {
        let dir = tempdir().unwrap();
        let p = write(dir.path(), "mystery.json", br#"{"schema":"other/v9"}"#);
        assert_eq!(detect(&p).unwrap(), InputKind::Log);
    }

    #[test]
    fn jsonl_and_malformed_objects_are_logs() {
        let dir = tempdir().unwrap();
        let jsonl = write(
            dir.path(),
            "events.jsonl",
            br#"{"level":"info","msg":"one"}
{"level":"warn","msg":"two"}
"#,
        );
        assert_eq!(detect(&jsonl).unwrap(), InputKind::Log);
        let broken = write(dir.path(), "truncated.log", br#"{"a": 1, "#);
        assert_eq!(detect(&broken).unwrap(), InputKind::Log);
    }

    #[test]
    fn renamed_compile_db_detects_past_the_sniff_window() {
        let dir = tempdir().unwrap();
        // First element's `file` key sits beyond the 64 KiB head read —
        // detection must stream the first array element, not rely on
        // the sniffed prefix.
        let padding = "x".repeat(200 * 1024);
        let db = format!(
            r#"[{{"directory":"/tmp","command":"cc -c {0}","file":"{0}"}}]"#,
            padding
        );
        let p = write(dir.path(), "renamed.json", db.as_bytes());
        assert_eq!(detect(&p).unwrap(), InputKind::CompileDb);
        // A renamed array without the compile shape is still a log.
        let not_db = write(
            dir.path(),
            "also_renamed.json",
            format!(r#"[{{"blob":"{padding}"}}]"#).as_bytes(),
        );
        assert_eq!(detect(&not_db).unwrap(), InputKind::Log);
    }

    #[test]
    fn plain_text_is_a_log() {
        let dir = tempdir().unwrap();
        let p = write(dir.path(), "app.log", b"INFO starting up\nWARN disk\n");
        assert_eq!(detect(&p).unwrap(), InputKind::Log);
    }

    #[test]
    fn detects_bundle() {
        let dir = tempdir().unwrap();
        let bundle = dir.path().join("b.lens");
        lens_core::create_bundle_archive(
            &bundle,
            "0.0.0-test",
            &[lens_core::BundleArtifact {
                name: "reports/x.json".to_string(),
                data: b"{}".to_vec(),
            }],
            Vec::new(),
            false,
        )
        .unwrap();
        assert_eq!(detect(&bundle).unwrap(), InputKind::Bundle);
    }

    #[test]
    fn missing_path_is_io_error() {
        assert!(detect(Path::new("/definitely/not/here")).is_err());
    }
}

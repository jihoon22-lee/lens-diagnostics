use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn lens(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lens"))
        .args(args)
        .output()
        .expect("failed to spawn lens")
}

/// Temp dir that is removed on drop, even when the test fails.
struct TestDir {
    _guard: tempfile::TempDir,
    path: PathBuf,
}

impl std::ops::Deref for TestDir {
    type Target = PathBuf;

    fn deref(&self) -> &PathBuf {
        &self.path
    }
}

fn tmp_dir(tag: &str) -> TestDir {
    let guard = tempfile::Builder::new()
        .prefix(&format!("lens-cli-test-{tag}-"))
        .tempdir()
        .unwrap();
    let path = guard.path().to_path_buf();
    TestDir {
        _guard: guard,
        path,
    }
}

fn tmp_file(tag: &str, name: &str, contents: &str) -> TestDir {
    let dir = tmp_dir(tag);
    let path = dir.path.join(name);
    std::fs::write(&path, contents).unwrap();
    TestDir { path, ..dir }
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

const JUNIT_XML: &str =
    r#"<testsuite tests="1"><testcase name="a" classname="C" time="0"/></testsuite>"#;

#[test]
fn stdout_pipe_closed_early_exits_cleanly() {
    // `lens net inspect --json | head -1`: the reader goes away mid-write.
    let mut child = Command::new(env!("CARGO_BIN_EXE_lens"))
        .args(["net", "inspect", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lens");

    let mut stdout = child.stdout.take().unwrap();
    let mut buf = [0u8; 256];
    let _ = stdout.read(&mut buf);
    drop(stdout);

    let status = child.wait().expect("wait");
    assert!(
        status.success(),
        "lens should exit 0 on a closed pipe, got {status:?}"
    );
}

#[test]
fn completion_closed_pipe_exits_cleanly() {
    // `lens completion bash | head -1`: the reader closes mid-write.
    let mut child = Command::new(env!("CARGO_BIN_EXE_lens"))
        .args(["completion", "bash"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lens");

    let mut stdout = child.stdout.take().unwrap();
    let mut buf = [0u8; 256];
    let _ = stdout.read(&mut buf);
    drop(stdout);

    let status = child.wait().expect("wait");
    assert!(
        status.success(),
        "lens completion should exit 0 on a closed pipe, got {status:?}"
    );
}

#[test]
fn doctor_root_scopes_preload_check() {
    // --root must read <root>/etc/ld.so.preload, not the host file.
    let root = tmp_dir("doctor-root");
    std::fs::create_dir_all(root.join("etc")).unwrap();
    std::fs::write(root.join("etc/ld.so.preload"), "/lib/evil.so\n").unwrap();

    let out = lens(&[
        "doctor",
        "--root",
        root.to_str().unwrap(),
        "--format",
        "json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("doctor JSON");
    let preload = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "ld.so.preload Verification")
        .expect("preload check present");
    assert_eq!(preload["status"], "WARN", "{preload}");
    assert!(
        preload["message"]
            .as_str()
            .unwrap()
            .contains("/lib/evil.so"),
        "{preload}"
    );

    // A root without the file reports "not present" whatever the host has.
    let empty = tmp_dir("doctor-empty");
    let out = lens(&[
        "doctor",
        "--root",
        empty.to_str().unwrap(),
        "--format",
        "json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("doctor JSON");
    let preload = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "ld.so.preload Verification")
        .expect("preload check present");
    assert_eq!(preload["status"], "PASS", "{preload}");
    assert!(
        preload["message"].as_str().unwrap().contains("not present"),
        "{preload}"
    );
}

#[test]
fn abi_non_elf_input_exits_2() {
    // Non-ELF input fails closed: exit 2, clear stderr, no report.
    let readme = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("README.md");

    let out = lens(&["abi", "inspect", readme.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("is not an ELF file"));
    assert!(!stdout_of(&out).contains("\"status\""));

    // Either side of a diff being non-ELF also errors.
    let elf = env!("CARGO_BIN_EXE_lens");
    for args in [
        ["abi", "diff", readme.to_str().unwrap(), elf],
        ["abi", "diff", elf, readme.to_str().unwrap()],
    ] {
        let out = lens(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr_of(&out));
        assert!(stderr_of(&out).contains("is not an ELF file"));
    }

    // A valid ELF still inspects cleanly.
    let out = lens(&["abi", "inspect", elf]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
}

#[test]
fn log_filter_since_until_window_and_exclusions() {
    // Mixed sources: ISO prefix, syslog (yearless), JSONL ts, and a
    // line with no timestamp.
    let dir = tmp_dir("since");
    let log = dir.join("app.log");
    std::fs::write(
        &log,
        "2026-10-09T10:00:00Z INFO early\n\
         2026-10-09T12:00:00Z INFO inside\n\
         Oct  9 13:00:00 host app: syslog line\n\
         {\"ts\":1791546720000,\"level\":\"info\",\"msg\":\"jsonl\"}\n\
         no timestamp at all INFO\n\
         2026-10-09T16:00:00Z INFO late\n",
    )
    .unwrap();

    // 11:00–14:00 UTC window: ISO + syslog(year=2026) + JSONL match;
    // the timestamp-less line is excluded and reported on stderr.
    // (ts 1791546720000 = 2026-10-09T12:12:00Z.)
    let out = lens(&[
        "log",
        "filter",
        log.to_str().unwrap(),
        "--since",
        "2026-10-09T11:00:00Z",
        "--until",
        "2026-10-09T14:00:00Z",
        "--year",
        "2026",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(!stdout.contains("early"), "{stdout}");
    assert!(stdout.contains("inside"), "{stdout}");
    assert!(stdout.contains("syslog line"), "{stdout}");
    assert!(stdout.contains("jsonl"), "{stdout}");
    assert!(!stdout.contains("late"), "{stdout}");
    assert!(!stdout.contains("no timestamp"), "{stdout}");
    assert!(
        stderr_of(&out).contains("timestamp could not be determined"),
        "{}",
        stderr_of(&out)
    );

    // --include-unknown restores the timestamp-less line.
    let out = lens(&[
        "log",
        "filter",
        log.to_str().unwrap(),
        "--since",
        "2026-10-09T11:00:00Z",
        "--until",
        "2026-10-09T14:00:00Z",
        "--year",
        "2026",
        "--include-unknown",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout_of(&out).contains("no timestamp"));

    // A different --year moves the syslog line out of the window.
    let out = lens(&[
        "log",
        "filter",
        log.to_str().unwrap(),
        "--since",
        "2026-10-09T11:00:00Z",
        "--until",
        "2026-10-09T14:00:00Z",
        "--year",
        "2020",
    ]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("inside"), "{stdout}");
    assert!(!stdout.contains("syslog line"), "{stdout}");

    // Space-separated form is accepted (UTC, no offset).
    let out = lens(&[
        "log",
        "filter",
        log.to_str().unwrap(),
        "--since",
        "2026-10-09 11:00:00",
        "--until",
        "2026-10-09 14:00:00",
        "--year",
        "2026",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("inside"), "{stdout}");
    assert!(stdout.contains("syslog line"), "{stdout}");

    // Date-only --until is midnight UTC → none of the 10:00+ lines.
    let out = lens(&[
        "log",
        "filter",
        log.to_str().unwrap(),
        "--until",
        "2026-10-09",
        "--year",
        "2026",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        !stdout_of(&out).contains("inside"),
        "date-only --until is midnight UTC"
    );

    // Invalid timestamps are usage errors → exit 2.
    for flag in ["--since", "--until"] {
        let out = lens(&["log", "filter", log.to_str().unwrap(), flag, "not-a-date"]);
        assert_eq!(out.status.code(), Some(2), "{flag}: {}", stderr_of(&out));
        assert!(stderr_of(&out).contains("invalid"), "{}", stderr_of(&out));
    }
}

#[test]
fn env_check_rejects_nonexistent_venv() {
    let out = lens(&["env", "check", "/nonexistent-venv-lens-test"]);
    assert_eq!(out.status.code(), Some(2), "errors must exit 2");
    assert!(
        stderr_of(&out).contains("virtualenv"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn env_check_rejects_non_venv_dir() {
    let dir = tmp_dir("notvenv");
    let out = lens(&["env", "check", dir.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "errors must exit 2");
    assert!(
        stderr_of(&out).contains("virtualenv"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn net_inspect_rejects_nonexistent_proc_dir() {
    let out = lens(&["net", "inspect", "--proc-dir", "/nonexistent-proc"]);
    assert_eq!(out.status.code(), Some(2), "errors must exit 2");
    assert!(!stderr_of(&out).is_empty());
}

#[test]
fn doctor_rejects_nonexistent_override_dirs() {
    let out = lens(&[
        "doctor",
        "--systemd-dir",
        "/nonexistent-systemd",
        "--procfs",
        "/nonexistent-proc",
    ]);
    assert_eq!(out.status.code(), Some(2), "errors must exit 2");
    assert!(
        stderr_of(&out).contains("does not exist"),
        "{}",
        stderr_of(&out)
    );

    let out = lens(&["doctor", "--procfs", "/nonexistent-proc"]);
    assert_eq!(out.status.code(), Some(2), "errors must exit 2");
    assert!(stderr_of(&out).contains("--procfs"), "{}", stderr_of(&out));
}

#[test]
fn test_parse_rejects_non_xml() {
    let file = tmp_file("junit", "report.xml", "not xml\n");
    let out = lens(&["test", "parse", file.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "errors must exit 2");
    assert!(stderr_of(&out).contains("JUnit"), "{}", stderr_of(&out));
}

#[test]
fn test_parse_accepts_junit_xml() {
    let file = tmp_file("junit-ok", "report.xml", JUNIT_XML);
    let out = lens(&["test", "parse", file.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr_of(&out));
}

#[test]
fn trace_analyze_rejects_non_strace() {
    let file = tmp_file(
        "trace",
        "notatrace.txt",
        "the quick brown fox\njumps over the lazy dog\npack my box with five dozen liquor jugs\n",
    );
    let out = lens(&["trace", "analyze", file.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "errors must exit 2");
    assert!(stderr_of(&out).contains("strace"), "{}", stderr_of(&out));
}

#[test]
fn bundle_create_isolates_source_failures() {
    let junit = tmp_file("bundle", "junit.xml", JUNIT_XML);
    let outdir = tmp_dir("bundle-out");
    let output = outdir.join("evidence.lens");

    let out = lens(&[
        "bundle",
        "create",
        output.to_str().unwrap(),
        "--test",
        junit.to_str().unwrap(),
        "--trace",
        "/nonexistent-trace",
    ]);
    // The bundle is still written, but a collection diagnostic is
    // incomplete evidence — a finding (exit 1).
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    assert!(output.exists());

    // The trace failure is preserved as a manifest diagnostic.
    let inspect = lens(&["bundle", "inspect", output.to_str().unwrap()]);
    assert!(inspect.status.success());
    let stdout = stdout_of(&inspect);
    assert!(stdout.contains("trace"), "{stdout}");

    // Refuse to overwrite without --force.
    let out = lens(&[
        "bundle",
        "create",
        output.to_str().unwrap(),
        "--test",
        junit.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2), "errors must exit 2");
    assert!(stderr_of(&out).contains("--force"), "{}", stderr_of(&out));

    // --force allows the overwrite.
    let out = lens(&[
        "bundle",
        "create",
        output.to_str().unwrap(),
        "--test",
        junit.to_str().unwrap(),
        "--force",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
}

#[cfg(unix)]
#[test]
fn disk_scan_reports_incomplete_on_unreadable_dirs() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tmp_dir("scan");
    let locked = dir.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = lens(&["disk", "scan", dir.to_str().unwrap()]);
    // When the filesystem honors the permission drop, the scan records an
    // error and the banner must admit the result is incomplete. (Running as
    // root bypasses the chmod — nothing to assert then.)
    if stderr_of(&out).contains("scan warning") {
        // Incomplete evidence is a finding: exit 1, not 0.
        assert_eq!(out.status.code(), Some(1));
        assert!(
            stdout_of(&out).contains("INCOMPLETE"),
            "{}",
            stdout_of(&out)
        );
    } else {
        assert!(out.status.success(), "{}", stderr_of(&out));
    }

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// --- Phase 1: accuracy ---

/// Build a minimal venv fixture: pyvenv.cfg + one dist-info per
/// (name, version, requires) tuple.
fn make_venv(tag: &str, dists: &[(&str, &str, &[&str])]) -> TestDir {
    let dir = tmp_dir(tag);
    std::fs::write(
        dir.join("pyvenv.cfg"),
        "home = /usr/bin\nversion = 3.14.4\n",
    )
    .unwrap();
    let sp = dir.join("lib/python3.14/site-packages");
    std::fs::create_dir_all(&sp).unwrap();
    for (name, version, requires) in dists {
        let di = sp.join(format!("{}-{}.dist-info", name, version));
        std::fs::create_dir_all(&di).unwrap();
        let mut meta = format!(
            "Metadata-Version: 2.1\nName: {}\nVersion: {}\n",
            name, version
        );
        for r in *requires {
            meta.push_str(&format!("Requires-Dist: {}\n", r));
        }
        std::fs::write(di.join("METADATA"), meta).unwrap();
    }
    dir
}

#[test]
fn env_check_evaluates_markers_and_extras() {
    let venv = make_venv(
        "env-markers",
        &[
            (
                "app",
                "1.0",
                &[
                    // python_version is 3.14 -> marker false -> skipped.
                    "importlib-metadata>=3.6.0; python_version < '3.10'",
                    // extra == "testing" inactive without --extras.
                    "pytest>=9.0; extra == 'testing'",
                    // Present but out of range -> conflict, not missing.
                    "werkzeug<3",
                ],
            ),
            ("werkzeug", "3.1.9", &[]),
        ],
    );

    let out = lens(&["env", "check", venv.to_str().unwrap()]);
    // Version conflicts are findings -> exit 1.
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(!stdout.contains("missing dependenc"), "{stdout}");
    assert!(stdout.contains("Version conflicts"), "{stdout}");

    // With the extra activated the unsatisfied dep surfaces.
    let out = lens(&[
        "env",
        "check",
        "--extras",
        "testing",
        venv.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("pytest"));
}

#[test]
fn log_filter_min_level_excludes_unknown_lines() {
    let log = tmp_file(
        "minlevel",
        "app.log",
        "2026-10-09 [ERROR] disk full\n\
         a plain line without any level\n\
         {\"level\":\"error\",\"msg\":\"gateway down\"}\n",
    );

    let out = lens(&[
        "log",
        "filter",
        log.to_str().unwrap(),
        "--min-level",
        "error",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("disk full"), "{stdout}");
    assert!(stdout.contains("gateway down"), "{stdout}");
    assert!(!stdout.contains("plain line"), "{stdout}");
    assert!(
        stderr_of(&out).contains("Excluded 1 line(s)"),
        "{}",
        stderr_of(&out)
    );

    let out = lens(&[
        "log",
        "filter",
        log.to_str().unwrap(),
        "--min-level",
        "error",
        "--include-unknown",
    ]);
    assert!(stdout_of(&out).contains("plain line"));
}

#[test]
fn trace_analyze_thread_shared_fds_and_errno_keys() {
    // F05 repro: a CLONE_FILES thread closes the shared fd 3, and fd 5
    // is O_CLOEXEC across execve — neither may appear as a leak.
    let trace = tmp_file(
        "f05",
        "t.strace",
        "100 openat(AT_FDCWD, \"/etc/hosts\", O_RDONLY) = 3\n\
         100 clone(child_stack=0x7f, flags=CLONE_VM|CLONE_FILES|CLONE_THREAD) = 101\n\
         101 close(3) = 0\n\
         101 +++ exited with 0 +++\n\
         100 openat(AT_FDCWD, \"/tmp/x\", O_RDONLY|O_CLOEXEC) = 5\n\
         100 execve(\"/bin/true\", [\"/bin/true\"], 0x55) = 0\n\
         100 openat(AT_FDCWD, \"/nope\", O_RDONLY) = -1 ENOENT (No such file or directory)\n\
         100 +++ exited with 0 +++\n",
    );

    let out = lens(&["trace", "analyze", trace.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let json: serde_json::Value =
        serde_json::from_str(&stdout_of(&out)).expect("analyze emits JSON");
    assert_eq!(json["fd_leaks"], serde_json::json!([]));
    assert!(
        json["fd_leaks_by_process"].is_null()
            || json["fd_leaks_by_process"].as_object().unwrap().is_empty(),
        "{}",
        json["fd_leaks_by_process"]
    );
    assert_eq!(json["errors"]["openat:ENOENT"], serde_json::json!(1));
}

#[test]
fn sys_cycles_report_directive_path_and_origin() {
    let dir = tmp_dir("sys-cycles");
    std::fs::write(
        dir.join("a.service"),
        "[Unit]\nBefore=b.service\nAfter=c.service\n[Service]\nExecStart=/bin/a\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.service"),
        "[Unit]\nBefore=c.service\n[Service]\nExecStart=/bin/b\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("c.service"),
        "[Unit]\n[Service]\nExecStart=/bin/c\n",
    )
    .unwrap();

    let out = lens(&["sys", "cycles", dir.to_str().unwrap()]);
    // A detected cycle is a finding -> exit 1.
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    // Real directed path annotated with directive(file:line) origin.
    assert!(stdout.contains("--Before(a.service:2)-->"), "{stdout}");
    assert!(stdout.contains("--Before(b.service:2)-->"), "{stdout}");
    assert!(stdout.contains("--After(a.service:3)-->"), "{stdout}");
    // Not the old alphabetical SCC join — in the domain output (the
    // trailing Findings block lists the member names, which is fine).
    let domain_output = stdout.split("Findings:").next().unwrap_or("");
    assert!(
        !domain_output.contains("a.service -> b.service -> c.service"),
        "{stdout}"
    );
}

#[test]
fn sys_diff_accepts_directories_and_sees_all_keys() {
    let base = tmp_dir("sys-diff-base");
    let cand = tmp_dir("sys-diff-cand");
    std::fs::write(base.join("a.service"), "[Service]\nExecStart=/bin/a\n").unwrap();
    std::fs::write(
        cand.join("a.service"),
        "[Service]\nUser=svc\nExecStart=/bin/a\n",
    )
    .unwrap();

    let out = lens(&[
        "sys",
        "diff",
        base.to_str().unwrap(),
        cand.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("diff JSON");
    let details = json["modified_units"][0]["details"]
        .as_str()
        .map(String::from);
    let details_json = json["modified_units"].to_string();
    assert!(
        details_json.contains("User"),
        "User= addition must appear in diff: {details_json}"
    );
    assert_eq!(json["modified_units"][0]["unit"], "a.service");
    let _ = details;
}

#[test]
fn abi_diff_import_only_change_is_compatible() {
    // Needs a C toolchain; self-skip when none is available.
    let dir = tmp_dir("abi-import");
    let src_v1 = dir.join("v1.c");
    let src_v2 = dir.join("v2.c");
    let so_v1 = dir.join("libx_v1.so");
    let so_v2 = dir.join("libx_v2.so");
    std::fs::write(
        &src_v1,
        "extern int helper_old(void);\n\
         __attribute__((weak)) int weak_fn(void) { return 1; }\n\
         int api_fn(void) { return helper_old(); }\n",
    )
    .unwrap();
    std::fs::write(
        &src_v2,
        "extern int helper_new(void);\n\
         __attribute__((weak)) int weak_fn(void) { return 1; }\n\
         int api_fn(void) { return helper_new(); }\n",
    )
    .unwrap();
    let ok = [(src_v1, so_v1.clone()), (src_v2, so_v2.clone())]
        .iter()
        .all(|(src, so)| {
            Command::new("cc")
                .args(["-shared", "-fPIC", "-o"])
                .arg(so)
                .arg(src)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        });
    if !ok {
        return;
    }

    // Weak symbol shows as weak in inspect evidence.
    let out = lens(&["abi", "inspect", so_v1.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let inspect: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("inspect JSON");
    let evidence = inspect["evidence"].as_array().unwrap();
    let weak = evidence
        .iter()
        .find(|s| s["identity"] == "weak_fn")
        .expect("weak_fn in evidence");
    assert_eq!(weak["binding"], "weak");
    // Imports are listed separately, not as exports.
    assert!(inspect["abi"]["imports"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s == "helper_old"));
    assert!(!inspect["abi"]["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s == "helper_old"));

    // An import-only change is compatible — it is not a removed export.
    let out = lens(&[
        "abi",
        "diff",
        so_v1.to_str().unwrap(),
        so_v2.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let diff: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("diff JSON");
    assert_eq!(diff["compatibility"], "compatible", "{diff}");
    assert!(diff["compatible"].as_bool().unwrap());
    assert!(diff["imports"]["added"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s == "helper_new"));
    assert!(diff["symbols"]["removed"].as_array().unwrap().is_empty());
}

#[test]
fn build_inspect_reports_transitive_impact() {
    let dir = tmp_dir("build-impact");
    let inc = dir.join("include");
    let src = dir.join("src");
    std::fs::create_dir_all(&inc).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(inc.join("leaf.h"), "#define L 1\n").unwrap();
    std::fs::write(src.join("mid.h"), "#include \"leaf.h\"\n").unwrap();
    std::fs::write(src.join("a.c"), "#include \"mid.h\"\n").unwrap();
    let cc_db = dir.join("compile_commands.json");
    std::fs::write(
        &cc_db,
        serde_json::to_string(&serde_json::json!([{
            "directory": dir.to_str().unwrap(),
            "file": src.join("a.c"),
            "arguments": ["cc", format!("-I{}", inc.display()), "-c", "a.c"],
        }]))
        .unwrap(),
    )
    .unwrap();

    let out = lens(&["build", "inspect", cc_db.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("inspect JSON");
    let leaf = inc.join("leaf.h").to_string_lossy().into_owned();
    let a_c = src.join("a.c").to_string_lossy().into_owned();
    // Direct includers (reverse_impact) do not list a.c for leaf.h.
    assert!(json["reverse_impact"].get(&leaf).is_none());
    // transitive_impact does — a.c reaches leaf.h through mid.h.
    assert!(
        json["transitive_impact"][&leaf]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| *u == a_c),
        "{}",
        json["transitive_impact"]
    );
}

#[test]
fn test_diff_regression_exits_1_clean_exits_0() {
    let base = tmp_file(
        "diff-base",
        "base.xml",
        r#"<testsuite tests="1"><testcase name="a" classname="C" time="0"/></testsuite>"#,
    );
    let regressed = tmp_file(
        "diff-cand",
        "cand.xml",
        r#"<testsuite tests="1"><testcase name="a" classname="C" time="0"><failure message="boom"/></testcase></testsuite>"#,
    );

    let out = lens(&[
        "test",
        "diff",
        base.to_str().unwrap(),
        regressed.to_str().unwrap(),
    ]);
    // A regression is a finding -> exit 1 (was 0 before the convention).
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));

    let out = lens(&[
        "test",
        "diff",
        base.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
}

// --- Phase 2: CLI contracts ---

#[test]
fn doctor_fail_on_warn_threshold() {
    // PATH="." triggers a PATH-sanity WARN; with an empty systemd dir and a
    // minimal fake procfs nothing else fails.
    let procfs = tmp_dir("procfs");
    let net = procfs.join("net");
    std::fs::create_dir_all(&net).unwrap();
    std::fs::write(
        net.join("tcp"),
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n",
    )
    .unwrap();
    let sysdir = tmp_dir("sys-empty");

    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_lens"))
            .args([
                "doctor",
                "--procfs",
                procfs.to_str().unwrap(),
                "--systemd-dir",
                sysdir.to_str().unwrap(),
            ])
            .args(extra)
            .env("PATH", ".")
            .output()
            .expect("failed to spawn lens")
    };

    let out = run(&[]);
    // WARN present: default --fail-on fail still exits 0.
    assert_eq!(out.status.code(), Some(0), "{}", stdout_of(&out));
    assert!(stdout_of(&out).contains("[WARN]"), "{}", stdout_of(&out));

    let out = run(&["--fail-on", "warn"]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout_of(&out));
}

#[test]
fn doctor_fail_on_fail_with_cycle() {
    let dir = tmp_dir("sys-cycle-doctor");
    std::fs::write(
        dir.join("a.service"),
        "[Unit]\nBefore=b.service\n[Service]\nExecStart=/bin/a\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.service"),
        "[Unit]\nBefore=a.service\n[Service]\nExecStart=/bin/b\n",
    )
    .unwrap();
    let procfs = tmp_dir("procfs2");
    let net = procfs.join("net");
    std::fs::create_dir_all(&net).unwrap();
    std::fs::write(
        net.join("tcp"),
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n",
    )
    .unwrap();
    let out = lens(&[
        "doctor",
        "--procfs",
        procfs.to_str().unwrap(),
        "--systemd-dir",
        dir.to_str().unwrap(),
    ]);
    assert!(stdout_of(&out).contains("[FAIL]"), "{}", stdout_of(&out));
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn test_parse_format_text_is_summary() {
    let f = tmp_file("parse-fmt", "r.xml", JUNIT_XML);
    let out = lens(&["test", "parse", f.to_str().unwrap(), "--format", "text"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("Test run:"), "{stdout}");
    assert!(!stdout.trim_start().starts_with('{'), "{stdout}");

    let out = lens(&["test", "parse", f.to_str().unwrap(), "--format", "json"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    assert_eq!(json["schema"], "testlens.run/v1");
}

#[test]
fn disk_scan_top_exclude_and_human_units() {
    let dir = tmp_dir("scan-top");
    let big = dir.join("big");
    let small = dir.join("small");
    let skipme = dir.join("skipme");
    for d in [&big, &small, &skipme] {
        std::fs::create_dir(d).unwrap();
    }
    std::fs::write(big.join("a.bin"), vec![0u8; 5 * 1024]).unwrap();
    std::fs::write(small.join("b.txt"), "x").unwrap();
    std::fs::write(skipme.join("c.bin"), vec![0u8; 9 * 1024]).unwrap();

    let out = lens(&[
        "disk",
        "scan",
        dir.to_str().unwrap(),
        "--top",
        "1",
        "--exclude",
        "skipme",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("KiB"), "{stdout}");
    assert!(stdout.contains("Largest top-level entries:"), "{stdout}");
    let top_section = stdout.split("Largest top-level entries:").nth(1).unwrap();
    assert!(top_section.contains("big"), "{stdout}");
    // --top 1 prints exactly one ranked entry.
    assert_eq!(
        top_section
            .lines()
            .filter(|l| l.starts_with("    "))
            .count(),
        1,
        "{stdout}"
    );
    // --exclude drop: skipme's 9 KiB must not be counted.
    assert!(!stdout.contains("skipme"), "{stdout}");

    // --max-depth 0 truncates at the root — but a user-requested limit is
    // a deliberate cut, not an evidence gap: complete stays true, exit 0.
    let out = lens(&[
        "disk",
        "scan",
        dir.to_str().unwrap(),
        "--max-depth",
        "0",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    assert_eq!(json["complete"], true, "{json}");
}

#[test]
fn disk_duplicates_json_text_and_reclaimable() {
    let dir = tmp_dir("dups");
    let blob = vec![7u8; 2048];
    std::fs::write(dir.join("a.bin"), &blob).unwrap();
    std::fs::write(dir.join("b.bin"), &blob).unwrap();
    std::fs::write(dir.join("unique.bin"), b"u").unwrap();

    let out = lens(&[
        "disk",
        "duplicates",
        dir.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    assert_eq!(json["groups"].as_array().unwrap().len(), 1);
    assert_eq!(json["total_reclaimable_bytes"], 2048);
    assert!(json["errors"].is_array());

    let out = lens(&["disk", "duplicates", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("Total reclaimable"), "{stdout}");
    assert!(stdout.contains("KiB"), "{stdout}");
}

#[test]
fn test_diff_multifile_and_structured_semantics() {
    let base = tmp_dir("base-run");
    let cand = tmp_dir("cand-run");
    std::fs::write(
        base.join("m1.xml"),
        r#"<testsuite name="s1" tests="2"><testcase name="keep" classname="C" time="0"/><testcase name="gone" classname="C" time="0"/></testsuite>"#,
    )
    .unwrap();
    std::fs::write(
        base.join("m2.xml"),
        r#"<testsuite name="s2" tests="1"><testcase name="sk" classname="C" time="0"><skipped/></testcase></testsuite>"#,
    )
    .unwrap();
    std::fs::write(
        cand.join("m1.xml"),
        r#"<testsuite name="s1" tests="1"><testcase name="keep" classname="C" time="0"/></testsuite>"#,
    )
    .unwrap();
    std::fs::write(
        cand.join("m2.xml"),
        r#"<testsuite name="s2" tests="2"><testcase name="sk" classname="C" time="0"/><testcase name="brand" classname="C" time="0"><error message="new boom"/></testcase></testsuite>"#,
    )
    .unwrap();

    let out = lens(&[
        "test",
        "diff",
        base.to_str().unwrap(),
        cand.to_str().unwrap(),
        "--format",
        "json",
    ]);
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    assert_eq!(json["schema"], "testlens.diff/v2");
    // skipped -> passed is a skip change, not a fix.
    assert_eq!(json["fixes"].as_array().unwrap().len(), 0);
    assert_eq!(json["skipped_changes"].as_array().unwrap().len(), 1);
    // Newly added failing test is a new_failure (a finding -> exit 1).
    assert_eq!(json["new_failures"].as_array().unwrap().len(), 1);
    assert_eq!(json["new_failures"][0]["message"], "new boom");
    // Removed case is surfaced.
    assert_eq!(json["removed_tests"].as_array().unwrap().len(), 1);
    assert_eq!(json["removed_tests"][0]["id"], "s1::C::gone");
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));

    // Glob input works the same way.
    let glob = format!("{}/*.xml", base.display());
    let out = lens(&["test", "diff", &glob, &glob]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
}

#[cfg(unix)]
#[test]
fn net_inspect_distinguishes_owner_unknown_from_orphan() {
    use std::os::unix::fs::PermissionsExt;

    let procfs = tmp_dir("procfs-net");
    let net = procfs.join("net");
    std::fs::create_dir_all(&net).unwrap();
    // One listening socket owned by our own uid.
    let uid = unsafe { libc::getuid() };
    std::fs::write(
        net.join("tcp"),
        format!(
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  {uid}        0 998877 1 0000000000000000 100 0 0 10 0\n"
        ),
    )
    .unwrap();
    // A process whose fd table cannot be read — its uid is the current
    // user's (the fake /proc tree is owned by us), matching the socket.
    let pid_dir = procfs.join("4321");
    std::fs::create_dir(&pid_dir).unwrap();
    std::fs::write(pid_dir.join("comm"), "svc").unwrap();
    let fd_dir = pid_dir.join("fd");
    std::fs::create_dir(&fd_dir).unwrap();
    std::fs::set_permissions(&fd_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = lens(&[
        "net",
        "inspect",
        "--proc-dir",
        procfs.to_str().unwrap(),
        "--format",
        "json",
    ]);
    std::fs::set_permissions(&fd_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    if json["summary"]["uninspectable_processes"]
        .as_u64()
        .unwrap_or(0)
        == 0
    {
        // Running as root or a filesystem that ignores mode bits — the
        // fixture cannot simulate EACCES; nothing to assert.
        return;
    }
    assert_eq!(json["summary"]["owner_unknown_sockets"], 1);
    assert_eq!(json["summary"]["orphan_sockets"], 0);
    assert_eq!(
        json["sockets"][0]["owner_state"],
        serde_json::json!("owner_unknown")
    );
}

#[test]
fn net_inspect_no_unix_and_text_owner_column() {
    let procfs = tmp_dir("procfs-net2");
    let net = procfs.join("net");
    std::fs::create_dir_all(&net).unwrap();
    std::fs::write(
        net.join("tcp"),
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  0        0 998877 1 0000000000000000 100 0 0 10 0\n",
    )
    .unwrap();
    std::fs::write(
        net.join("unix"),
        "Num       RefCount Protocol Flags    Type St Inode Path\n0000000000000000: 00000002 00000000 00010000 0001 01 123456 /run/x.sock\n",
    )
    .unwrap();

    let out = lens(&[
        "net",
        "inspect",
        "--proc-dir",
        procfs.to_str().unwrap(),
        "--format",
        "json",
        "--no-unix",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    assert_eq!(json["sockets"].as_array().unwrap().len(), 1);
    assert_eq!(json["sockets"][0]["kind"], "tcp");

    // Text mode renders the unowned socket as <orphan>.
    let out = lens(&["net", "inspect", "--proc-dir", procfs.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("<orphan>"), "{}", stdout_of(&out));
}

// --- Phase 2b: workflow depth ---

#[test]
fn log_filter_shows_lossy_utf8_lines() {
    // F20: a line containing invalid UTF-8 must still be indexed, matched,
    // and counted — not silently skipped.
    let dir = tmp_dir("lossy");
    let log = dir.join("app.log");
    let mut bytes = b"[INFO] clean start\n".to_vec();
    bytes.extend_from_slice(b"[ERROR] disk \xff\xfe corrupted\n");
    bytes.extend_from_slice(b"[INFO] clean end\n");
    std::fs::write(&log, bytes).unwrap();

    let out = lens(&["log", "filter", log.to_str().unwrap(), "--query", "disk"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("disk"), "{stdout}");
    assert!(
        stderr_of(&out).contains("invalid UTF-8"),
        "{}",
        stderr_of(&out)
    );

    let out = lens(&["log", "filter", log.to_str().unwrap(), "--format", "json"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    assert_eq!(json["lossy_lines"], serde_json::json!(1));
    assert_eq!(json["total"], serde_json::json!(3));
}

#[test]
fn log_filter_reads_gzip_input() {
    let dir = tmp_dir("gz");
    let plain = dir.join("app.log");
    let gz = dir.join("app.log.gz");
    std::fs::write(&plain, "[INFO] alpha\n[ERROR] beta\n").unwrap();
    let ok = Command::new("gzip")
        .arg("-k")
        .arg(&plain)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return; // no gzip binary on this host
    }
    assert!(gz.exists());

    let out = lens(&["log", "filter", gz.to_str().unwrap(), "--query", "beta"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("beta"), "{}", stdout_of(&out));
    assert!(
        stderr_of(&out).contains("Matched 1 of 2"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn log_filter_reads_stdin_dash() {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_lens"))
        .args(["log", "filter", "-", "--query", "needle"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lens");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hay\na needle here\nmore hay\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("needle"), "{}", stdout_of(&out));
    assert!(
        stderr_of(&out).contains("Matched 1 of 3"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn log_filter_regex_limit_context_jsonl() {
    let log = tmp_file(
        "logflags",
        "app.log",
        "start\nnoise 1\nERROR code=42 here\nnoise 2\nnoise 3\nERROR code=99\nnoise 4\n",
    );

    // --regex matches a pattern; --limit stops early; --context shows
    // surrounding lines; --format jsonl emits one object per line.
    let out = lens(&[
        "log",
        "filter",
        log.to_str().unwrap(),
        "--regex",
        r"code=\d+",
        "--limit",
        "1",
        "--context",
        "1",
        "--format",
        "jsonl",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("jsonl line"))
        .collect();
    assert_eq!(lines.len(), 3, "{stdout}"); // before, match, after
    assert_eq!(lines[0]["context"], true);
    assert_eq!(lines[1]["context"], false);
    assert!(lines[1]["text"].as_str().unwrap().contains("code=42"));
    assert_eq!(lines[2]["context"], true);
    assert!(stderr_of(&out).contains("--limit"), "{}", stderr_of(&out));

    // Invalid regex is a usage error, not a silent no-op.
    let out = lens(&[
        "log",
        "filter",
        log.to_str().unwrap(),
        "--regex",
        "([unclosed",
    ]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
}

#[test]
fn trash_multi_path_dry_run_list_restore() {
    let dir = tmp_dir("trash");
    let xdg = tmp_dir("trash-xdg");
    let a = dir.join("a.txt");
    let b = dir.join("b.txt");
    std::fs::write(&a, "aaa").unwrap();
    std::fs::write(&b, "bbb").unwrap();

    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_lens"))
            .args(args)
            .env("XDG_DATA_HOME", xdg.as_path())
            .env("HOME", "/nonexistent-home-lens-test")
            .output()
            .expect("spawn lens")
    };

    // --dry-run reports intent without moving anything.
    let out = run(&["disk", "trash", a.to_str().unwrap(), "--dry-run"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("would trash"),
        "{}",
        stdout_of(&out)
    );
    assert!(a.exists(), "dry-run must not move the file");

    // Multiple paths in one invocation.
    let out = run(&["disk", "trash", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(!a.exists() && !b.exists());

    // trash list shows both entries with original paths.
    let out = run(&["disk", "trash", "list"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("a.txt"), "{stdout}");
    assert!(stdout.contains("b.txt"), "{stdout}");

    // trash restore puts the file back; identity sidecar must verify.
    let out = run(&["disk", "trash", "restore", "a.txt"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(a.exists());
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "aaa");

    // A bogus name is rejected (fail closed).
    let out = run(&["disk", "trash", "restore", "../escape"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
}

#[test]
fn build_impact_resolves_relative_header_and_hints() {
    // F16: --header relative paths resolve against the compile database
    // directory (and entry directories), not just the process cwd.
    let dir = tmp_dir("build-f16");
    let inc = dir.join("include");
    let src = dir.join("src");
    std::fs::create_dir_all(&inc).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(inc.join("common.h"), "#define C 1\n").unwrap();
    std::fs::write(src.join("a.c"), "#include \"common.h\"\n").unwrap();
    let cc_db = dir.join("compile_commands.json");
    std::fs::write(
        &cc_db,
        serde_json::to_string(&serde_json::json!([{
            "directory": dir.to_str().unwrap(),
            "file": src.join("a.c"),
            "arguments": ["cc", format!("-I{}", inc.display()), "-c", "a.c"],
        }]))
        .unwrap(),
    )
    .unwrap();

    // Run from an unrelated cwd: --header include/common.h must still hit.
    let cwd = tmp_dir("build-f16-cwd");
    let out = Command::new(env!("CARGO_BIN_EXE_lens"))
        .args([
            "build",
            "impact",
            cc_db.to_str().unwrap(),
            "--header",
            "include/common.h",
        ])
        .current_dir(cwd.as_path())
        .output()
        .expect("spawn lens");
    assert!(out.status.success(), "{}", stderr_of(&out));
    let json: serde_json::Value =
        serde_json::from_str(&stdout_of(&out)).expect("impact JSON (default format)");
    assert_eq!(json["total_impacted"], serde_json::json!(1));

    // A header absent from the graph prints a warning hint but stays a
    // clean (findings-free) exit.
    let out = lens(&[
        "build",
        "impact",
        cc_db.to_str().unwrap(),
        "--header",
        "common.h", // basename matches a known header's tail
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("not in the include graph"),
        "{}",
        stderr_of(&out)
    );
    // Missing include directives surface as a warning.
    let out = lens(&[
        "build",
        "impact",
        cc_db.to_str().unwrap(),
        "--header",
        "include/common.h",
        "--format",
        "json",
    ]);
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("impact JSON");
    assert!(json["unresolved_includes"].is_u64(), "{json}");
    assert!(json["missing_sources"].is_u64(), "{json}");
}

#[test]
fn bundle_log_content_show_and_extract() {
    // F22: --log embeds raw log content; show reads an entry without tar;
    // extract verifies first and refuses overwrite without --force.
    let dir = tmp_dir("bundle-f22");
    let log = dir.join("incident.log");
    std::fs::write(&log, "[ERROR] first\n[INFO] second\n").unwrap();
    let output = dir.join("evidence.lens");

    let out = lens(&[
        "bundle",
        "create",
        output.to_str().unwrap(),
        "--log",
        log.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    let inspect = lens(&["bundle", "inspect", output.to_str().unwrap()]);
    let stdout = stdout_of(&inspect);
    assert!(stdout.contains("logs/incident.log"), "{stdout}");

    // bundle show prints raw entry content without external tar.
    let out = lens(&[
        "bundle",
        "show",
        output.to_str().unwrap(),
        "logs/incident.log",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("[ERROR] first"), "{stdout}");

    // extract verifies, writes files, and refuses overwrite.
    let dest = dir.join("out");
    let out = lens(&[
        "bundle",
        "extract",
        output.to_str().unwrap(),
        dest.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(dest.join("logs/incident.log").exists());
    assert!(dest.join("manifest.json").exists());
    assert_eq!(
        std::fs::read_to_string(dest.join("logs/incident.log")).unwrap(),
        "[ERROR] first\n[INFO] second\n"
    );

    let out = lens(&[
        "bundle",
        "extract",
        output.to_str().unwrap(),
        dest.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("--force"), "{}", stderr_of(&out));

    let out = lens(&[
        "bundle",
        "extract",
        output.to_str().unwrap(),
        dest.to_str().unwrap(),
        "--force",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    // A tampered bundle is an integrity finding (exit 1), not "crypto".
    // The archive must still decompress cleanly with altered content, so
    // rebuild it with python3's tarfile (self-skip when python3 is absent).
    let bad = dir.join("tampered.lens");
    let script = [
        "import tarfile, io".to_string(),
        format!("t = tarfile.open({:?}, 'r:gz')", output.to_str().unwrap()),
        "members = [(m.name, t.extractfile(m).read()) for m in t.getmembers() if m.isfile()]"
            .to_string(),
        "t.close()".to_string(),
        format!("t = tarfile.open({:?}, 'w:gz')", bad.to_str().unwrap()),
        "for name, data in members:".to_string(),
        "    if name == 'logs/incident.log': data = b'forged'".to_string(),
        "    ti = tarfile.TarInfo(name); ti.size = len(data)".to_string(),
        "    t.addfile(ti, io.BytesIO(data))".to_string(),
        "t.close()".to_string(),
    ]
    .join("\n");
    let rebuilt = Command::new("python3")
        .args(["-c", &script])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if rebuilt {
        let out = lens(&["bundle", "verify", bad.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
        assert!(stderr_of(&out).contains("integrity"), "{}", stderr_of(&out));
        // Extract refuses to unpack an unverifiable bundle.
        let out = lens(&[
            "bundle",
            "extract",
            bad.to_str().unwrap(),
            dir.join("out2").to_str().unwrap(),
        ]);
        assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    }
}

#[test]
fn sys_inspect_without_path_uses_search_dirs() {
    // F21: no explicit path merges the systemd search path. On any host
    // this must be a clean run or findings — never a usage error.
    let out = lens(&["sys", "inspect"]);
    let code = out.status.code().unwrap_or(-1);
    assert!(
        code == 0 || code == 1,
        "expected 0/1, got {}: {}",
        code,
        stderr_of(&out)
    );
    let out = lens(&["sys", "cycles"]);
    let code = out.status.code().unwrap_or(-1);
    assert!(
        code == 0 || code == 1,
        "expected 0/1, got {}: {}",
        code,
        stderr_of(&out)
    );
}

#[test]
fn usage_errors_do_not_say_corrupt_input() {
    // F25: a missing-flag usage error is reported as usage, not
    // "Corrupt or invalid input format:".
    let dir = tmp_dir("usage");
    let out_path = dir.join("empty.lens");
    let out = lens(&["bundle", "create", out_path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("at least one artifact source"),
        "{}",
        stderr
    );
    assert!(
        !stderr.contains("Corrupt or invalid input format"),
        "{}",
        stderr
    );
}

#[test]
fn json_parse_error_names_the_file() {
    // F25: a malformed JSON input reports which file failed to parse.
    let bad = tmp_file("badjson", "cc.json", "not json {");
    let out = lens(&["build", "inspect", bad.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_of(&out);
    assert!(stderr.contains("cc.json"), "{}", stderr);
}

#[test]
fn help_documents_positional_args() {
    // F25: positional arguments carry descriptions, not bare <PATH>.
    for args in [
        vec!["disk", "scan", "--help"],
        vec!["log", "filter", "--help"],
        vec!["abi", "inspect", "--help"],
    ] {
        let out = lens(&args);
        assert!(out.status.success());
        let help = stdout_of(&out);
        for line in help.lines() {
            // Any listed positional has a description after it.
            if line.trim_start().starts_with('<') {
                assert!(
                    line.contains('<') && line.matches('<').count() >= 1 && line.len() > 25,
                    "undocumented arg line: {line}"
                );
            }
        }
    }
}

#[test]
fn sys_inspect_reports_parser_diagnostics() {
    // F28: garbage lines, unclosed section headers, and references to
    // missing units surface in snapshot diagnostics.
    let dir = tmp_dir("sysdiag");
    std::fs::write(
        dir.join("bad.service"),
        "[Unit\nRequires=nope.service\ngarbage line\n[Service]\nExecStart=/bin/x\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("refs.service"),
        "[Unit]\nRequires=nope.service\n[Service]\nExecStart=/bin/y\n",
    )
    .unwrap();

    let out = lens(&["sys", "inspect", dir.to_str().unwrap(), "--format", "json"]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("snapshot json");
    let diags = v["diagnostics"].as_array().expect("diagnostics array");
    let codes: Vec<&str> = diags.iter().filter_map(|d| d["code"].as_str()).collect();
    assert!(codes.contains(&"SYNTAX_SECTION_HEADER"), "{codes:?}");
    assert!(codes.contains(&"SYNTAX_GARBAGE_LINE"), "{codes:?}");
    assert!(codes.contains(&"UNIT_REF_MISSING"), "{codes:?}");
}

#[test]
fn env_check_flags_top_level_shadowing_only() {
    // F27: `pkg/json.py` must not flag stdlib `json`; a package dir
    // `logging/__init__.py` must flag; `secrets.py` is in the stdlib list.
    let dir = tmp_dir("shadow");
    let venv = dir.join("venv");
    std::fs::create_dir_all(venv.join("lib/python3.12/site-packages")).unwrap();
    std::fs::write(venv.join("pyvenv.cfg"), "version = 3.12.0\n").unwrap();
    let proj = dir.join("proj");
    std::fs::create_dir_all(proj.join("pkg")).unwrap();
    std::fs::write(proj.join("pkg/__init__.py"), "").unwrap();
    std::fs::write(proj.join("pkg/json.py"), "x=1\n").unwrap();
    std::fs::create_dir_all(proj.join("logging")).unwrap();
    std::fs::write(proj.join("logging/__init__.py"), "x=1\n").unwrap();
    std::fs::write(proj.join("secrets.py"), "x=1\n").unwrap();

    let out = lens(&[
        "env",
        "inspect",
        venv.to_str().unwrap(),
        "--project",
        proj.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("env snapshot json");
    let names: Vec<&str> = v["venv"]["shadowing_issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["module_name"].as_str())
        .collect();
    assert_eq!(names, vec!["logging", "secrets"], "{names:?}");
}

// --- Phase 3: review-hardening regressions ---

#[test]
fn bundle_test_accepts_junit_directory() {
    // --test documented as "file/directory": a directory must parse every
    // *.xml inside instead of failing with "Is a directory".
    let dir = tmp_dir("bundle-testdir");
    let junit = dir.join("cases");
    std::fs::create_dir(&junit).unwrap();
    std::fs::write(
        junit.join("a.xml"),
        r#"<testsuite tests="1"><testcase name="a" classname="C" time="0"/></testsuite>"#,
    )
    .unwrap();
    std::fs::write(
        junit.join("b.xml"),
        r#"<testsuite tests="1"><testcase name="b" classname="C" time="0"/></testsuite>"#,
    )
    .unwrap();
    let output = dir.join("evidence.lens");

    let out = lens(&[
        "bundle",
        "create",
        output.to_str().unwrap(),
        "--test",
        junit.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));

    let out = lens(&[
        "bundle",
        "show",
        output.to_str().unwrap(),
        "reports/test_run.json",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let run: serde_json::Value =
        serde_json::from_str(&stdout_of(&out)).expect("test_run.json parses");
    assert_eq!(run["summary"]["total"], 2, "{run}");
}

fn gzip_file(src: &PathBuf, dst: &PathBuf) -> bool {
    let out = Command::new("gzip")
        .arg("-c")
        .arg(src)
        .output()
        .expect("spawn gzip");
    if !out.status.success() {
        return false;
    }
    std::fs::write(dst, &out.stdout).unwrap();
    true
}

#[test]
fn bundle_log_gz_embeds_decompressed_tail() {
    let dir = tmp_dir("bundle-gzbig");
    let plain = dir.join("big.log");
    // >8 MiB decompressed so the embedded tail is truncated (a
    // collection diagnostic -> exit 1 under the shared rules).
    let mut content = String::new();
    for i in 0..400_000 {
        content.push_str(&format!("[INFO] line {i} padding padding padding\n"));
    }
    let last = "[ERROR] final line of the log\n";
    content.push_str(last);
    assert!(content.len() > 8 * 1024 * 1024);
    std::fs::write(&plain, &content).unwrap();
    let gz = dir.join("big.log.gz");
    if !gzip_file(&plain, &gz) {
        return; // no gzip binary on this host
    }

    let output = dir.join("big.lens");
    let out = lens(&[
        "bundle",
        "create",
        output.to_str().unwrap(),
        "--log",
        gz.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));

    // The manifest records the truncation diagnostic.
    let inspect = lens(&[
        "bundle",
        "inspect",
        output.to_str().unwrap(),
        "--format",
        "json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&inspect)).expect("inspect JSON");
    let diags = v["manifest"]["diagnostics"].to_string();
    assert!(diags.contains("truncated"), "{diags}");

    // Extracted entry is logs/big.log (suffix dropped) — valid plain
    // text, not a corrupt gzip tail.
    let dest = dir.join("out");
    let out = lens(&[
        "bundle",
        "extract",
        output.to_str().unwrap(),
        dest.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let embedded = std::fs::read(dest.join("logs/big.log")).unwrap();
    assert_ne!(&embedded[..2], &[0x1f, 0x8b], "must not be gzip data");
    assert!(embedded.ends_with(last.as_bytes()));
}

#[test]
fn bundle_log_gz_small_embeds_full_content() {
    let dir = tmp_dir("bundle-gzsmall");
    let plain = dir.join("app.log");
    let content = "[INFO] alpha\n[ERROR] beta\n";
    std::fs::write(&plain, content).unwrap();
    let gz = dir.join("app.log.gz");
    if !gzip_file(&plain, &gz) {
        return; // no gzip binary on this host
    }

    let output = dir.join("small.lens");
    let out = lens(&[
        "bundle",
        "create",
        output.to_str().unwrap(),
        "--log",
        gz.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));

    let out = lens(&["bundle", "show", output.to_str().unwrap(), "logs/app.log"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out).trim_end(), content.trim_end());
}

#[test]
fn bundle_log_stdin_dash_embeds_stdin() {
    use std::io::Write;
    let dir = tmp_dir("bundle-stdin");
    let output = dir.join("stdin.lens");
    let mut child = Command::new(env!("CARGO_BIN_EXE_lens"))
        .args(["bundle", "create", output.to_str().unwrap(), "--log", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lens");
    child.stdin.take().unwrap().write_all(b"a\nb\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));

    let out = lens(&["bundle", "show", output.to_str().unwrap(), "logs/stdin.log"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out).trim_end(), "a\nb");

    // No content-collection diagnostic: stdin was read, not a literal
    // file named "-".
    let inspect = lens(&[
        "bundle",
        "inspect",
        output.to_str().unwrap(),
        "--format",
        "json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&inspect)).expect("inspect JSON");
    let diags = v["manifest"]["diagnostics"].to_string();
    assert!(!diags.contains("log content"), "{diags}");
}

#[test]
fn trace_diff_rejects_non_strace() {
    // The same fail-closed gate `trace analyze` uses applies to both
    // inputs of `trace diff`.
    let md = tmp_file(
        "trace-md",
        "notes.md",
        "# Notes\n\n- some *markdown* text\n- more lines\n- and more\n- and more\n- and more\n",
    );
    let out = lens(&["trace", "diff", md.to_str().unwrap(), md.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("does not look like strace output"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn bundle_trace_rejects_non_strace_via_diagnostics() {
    let dir = tmp_dir("bundle-trace-md");
    let md = dir.join("notes.md");
    std::fs::write(
        &md,
        "# Notes\n\n- some *markdown* text\n- more lines\n- and more\n- and more\n",
    )
    .unwrap();
    let log = dir.join("ok.log");
    std::fs::write(&log, "[INFO] fine\n").unwrap();
    let output = dir.join("evidence.lens");

    let out = lens(&[
        "bundle",
        "create",
        output.to_str().unwrap(),
        "--trace",
        md.to_str().unwrap(),
        "--log",
        log.to_str().unwrap(),
    ]);
    // The bundle is still created (the log source worked), the trace
    // failure is a manifest diagnostic, and no snapshot is embedded —
    // but the diagnostic makes the run a finding (exit 1).
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));

    let inspect = lens(&[
        "bundle",
        "inspect",
        output.to_str().unwrap(),
        "--format",
        "json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&inspect)).expect("inspect JSON");
    let diags = v["manifest"]["diagnostics"].to_string();
    assert!(
        diags.contains("does not look like strace output"),
        "{diags}"
    );
    let names: Vec<&str> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(!names.contains(&"reports/trace_snapshot.json"), "{names:?}");
}

#[test]
fn doctor_root_empty_dir_skips_host_checks() {
    // --root on an empty image must not report host evidence: network,
    // services and PATH checks all SKIP.
    let root = tmp_dir("doctor-fakeroot");
    let out = lens(&[
        "doctor",
        "--root",
        root.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert!(out.status.code().is_some(), "spawn failed");
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("doctor JSON");
    let checks = v["checks"].as_array().unwrap();
    let status_of = |name: &str| {
        checks
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("missing check {name}"))["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(status_of("Network"), "SKIP", "{v}");
    assert_eq!(status_of("Systemd Dependency Cycles"), "SKIP", "{v}");
    assert_eq!(status_of("PATH Sanity"), "SKIP", "{v}");
    assert_eq!(status_of("ld.so.preload Verification"), "PASS", "{v}");
    assert_eq!(v["summary"]["skipped"], 3, "{v}");
    for c in checks {
        assert!(
            !c["message"]
                .as_str()
                .unwrap_or("")
                .contains("Total sockets"),
            "host socket data leaked into --root report: {c}"
        );
    }
}

#[test]
fn log_filter_buffered_output_still_correct() {
    // Buffered stdout must not reorder, drop, or corrupt lines — 50k
    // matches exercise multiple flush cycles.
    let dir = tmp_dir("filter-buffered");
    let log = dir.join("big.log");
    let n = 50_000;
    let mut content = String::new();
    for i in 0..n {
        content.push_str(&format!("[INFO] msg {i}\n"));
    }
    std::fs::write(&log, &content).unwrap();

    let out = lens(&["log", "filter", log.to_str().unwrap(), "--query", "msg"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert_eq!(
        stdout.lines().count(),
        n,
        "{stderr}",
        stderr = stderr_of(&out)
    );

    let out = lens(&[
        "log",
        "filter",
        log.to_str().unwrap(),
        "--query",
        "msg",
        "--format",
        "jsonl",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    let mut count = 0usize;
    for l in stdout.lines() {
        let v: serde_json::Value = serde_json::from_str(l).expect("jsonl line parses");
        assert!(v["text"].is_string(), "{l}");
        count += 1;
    }
    assert_eq!(count, n);
}

// --- Shared ops layer: centralized finding -> exit-code mapping ---

/// A minimal valid lens.net/v1 report with the given listeners
/// (`(local_port, process_name)`), constructed directly so the test
/// does not depend on the host's /proc.
fn net_report_json(listeners: &[(u32, &str)]) -> String {
    let listening: Vec<String> = listeners
        .iter()
        .enumerate()
        .map(|(i, (port, _proc))| {
            format!(
                r#"{{"kind":"tcp","local_address":"0.0.0.0","local_port":{port},"remote_address":"","remote_port":0,"state":"LISTEN","inode":{},"uid":0,"tx_queue":0,"rx_queue":0,"process":null}}"#,
                1000 + i
            )
        })
        .collect();
    format!(
        r#"{{"schema_version":"lens.net/v1","summary":{{"total_sockets":{n},"listening_ports":{n},"established_connections":0,"time_wait_sockets":0,"orphan_sockets":0,"unix_domain_sockets":0}},"sockets":[],"listening":[{listening}]}}"#,
        n = listeners.len(),
        listening = listening.join(",")
    )
}

#[test]
fn net_diff_new_listener_exits_1_and_prints_findings() {
    let dir = tmp_dir("netdiff");
    let base = dir.join("base.json");
    let cand = dir.join("cand.json");
    std::fs::write(&base, net_report_json(&[(80, "nginx")])).unwrap();
    std::fs::write(
        &cand,
        net_report_json(&[(80, "nginx"), (4444, "cryptominer")]),
    )
    .unwrap();

    let out = lens(&[
        "net",
        "diff",
        base.to_str().unwrap(),
        cand.to_str().unwrap(),
        "--format",
        "text",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    // Text mode renders a Findings block after the domain output.
    let stdout = stdout_of(&out);
    assert!(stdout.contains("Findings:"), "{stdout}");
    assert!(stdout.contains("[FAIL] net.new_listener"), "{stdout}");
    assert!(stdout.contains("4444"), "{stdout}");
}

#[test]
fn net_diff_json_output_has_no_findings_key() {
    let dir = tmp_dir("netjson");
    let base = dir.join("base.json");
    let cand = dir.join("cand.json");
    std::fs::write(&base, net_report_json(&[(80, "nginx")])).unwrap();
    std::fs::write(
        &cand,
        net_report_json(&[(80, "nginx"), (4444, "cryptominer")]),
    )
    .unwrap();

    let out = lens(&[
        "net",
        "diff",
        base.to_str().unwrap(),
        cand.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    assert!(
        json.get("findings").is_none(),
        "JSON output must stay the plain domain shape"
    );
    assert_eq!(json["new_listeners"].as_array().unwrap().len(), 1);
}

#[test]
fn sys_diff_new_cycle_exits_1() {
    let dir = tmp_dir("syscycle");
    let base_dir = dir.join("base");
    let cand_dir = dir.join("cand");
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::create_dir_all(&cand_dir).unwrap();
    std::fs::write(base_dir.join("a.service"), "[Unit]\n[Service]\n").unwrap();
    std::fs::write(
        cand_dir.join("a.service"),
        "[Unit]\nAfter=b.service\n[Service]\n",
    )
    .unwrap();
    std::fs::write(
        cand_dir.join("b.service"),
        "[Unit]\nAfter=a.service\n[Service]\n",
    )
    .unwrap();

    let out = lens(&[
        "sys",
        "diff",
        base_dir.to_str().unwrap(),
        cand_dir.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("new_cycles"));
}

#[test]
fn trace_diff_new_error_exits_1() {
    let dir = tmp_dir("tracediff");
    let base = dir.join("base.strace");
    let cand = dir.join("cand.strace");
    let shared = "1 openat(AT_FDCWD, \"/etc/passwd\", O_RDONLY) = 3\n1 close(3) = 0\n";
    std::fs::write(&base, shared).unwrap();
    std::fs::write(
        &cand,
        format!("{shared}1 read(3, 0x7fff, 4096) = -1 EIO (Input/output error)\n"),
    )
    .unwrap();

    let out = lens(&[
        "trace",
        "diff",
        base.to_str().unwrap(),
        cand.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("new_errors"));
}

#[test]
fn test_parse_incomplete_junit_exits_1() {
    let dir = tmp_dir("junittrunc");
    let f = dir.join("truncated.xml");
    // Boundary-truncated after a complete testcase element.
    std::fs::write(
        &f,
        r#"<testsuite tests="2" failures="0"><testcase classname="a" name="ok" time="0.1"/>"#,
    )
    .unwrap();
    let out = lens(&["test", "parse", f.to_str().unwrap(), "--format", "json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).unwrap();
    assert_eq!(json["complete"], false);
}

#[test]
fn bundle_create_with_collection_warnings_exits_1_but_writes_bundle() {
    let dir = tmp_dir("bundlewarn");
    let log = tmp_file("bundlewarn", "ok.log", "[INFO] fine\n");
    let output = dir.join("warn.lens");
    let out = lens(&[
        "bundle",
        "create",
        output.to_str().unwrap(),
        "--log",
        log.to_str().unwrap(),
        "--env",
        "/nonexistent-venv",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    assert!(output.exists(), "bundle written despite the warning");

    let show = lens(&["bundle", "show", output.to_str().unwrap(), "logs/ok.log"]);
    assert_eq!(stdout_of(&show).trim_end(), "[INFO] fine");
}

#[test]
fn findings_block_caps_detail_lines_at_20() {
    // 25 regressions → 25 Fail findings; the text block must cap the
    // detail list at 20 and point at JSON for the rest.
    let dir = tmp_dir("findingscap");
    let cases = |suffix: &str| {
        (0..25)
            .map(
                |i| format!(r#"<testcase classname="C" name="t{i}" time="0">{suffix}</testcase>"#,),
            )
            .collect::<String>()
    };
    let base = dir.join("base.xml");
    let cand = dir.join("cand.xml");
    std::fs::write(
        &base,
        format!("<testsuite tests=\"25\">{}</testsuite>", cases("")),
    )
    .unwrap();
    std::fs::write(
        &cand,
        format!(
            "<testsuite tests=\"25\">{}</testsuite>",
            cases("<failure>oops</failure>")
        ),
    )
    .unwrap();

    let out = lens(&[
        "test",
        "diff",
        base.to_str().unwrap(),
        cand.to_str().unwrap(),
        "--format",
        "text",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("Findings: 25 fail"), "{stdout}");
    let detail = stdout
        .lines()
        .filter(|l| l.contains("[FAIL]") || l.contains("[WARN]"))
        .count();
    assert_eq!(detail, 20, "{stdout}");
    assert!(
        stdout.contains("… and 5 more (use --format json for the full report)"),
        "{stdout}"
    );
}

#[test]
fn env_diff_rejects_wrong_schema_input() {
    let dir = tmp_dir("envschema");
    let base = dir.join("base.json");
    let cand = dir.join("cand.json");
    // Feed env diff a *net* report: schema validation must reject it
    // before diffing (usage/schema failure -> exit 2).
    std::fs::write(&base, net_report_json(&[])).unwrap();
    std::fs::write(&cand, net_report_json(&[])).unwrap();

    let out = lens(&[
        "env",
        "diff",
        base.to_str().unwrap(),
        cand.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("expected schema"),
        "{}",
        stderr_of(&out)
    );
}

// --- Phase 3: global options + auto-detection ---

/// Strip runtime fields so two invocations of the same command can be
/// compared byte-for-byte.
fn normalize_volatile(s: &str) -> String {
    s.lines()
        .filter(|l| !l.contains("\"collected_at\"") && !l.contains("\"run_id\""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn inspect_junit_matches_test_parse() {
    let dir = tmp_file("auto-inspect", "r.xml", JUNIT_XML);
    let file = dir.path.clone();
    let auto = lens(&["inspect", file.to_str().unwrap()]);
    let manual = lens(&["test", "parse", file.to_str().unwrap()]);
    assert_eq!(auto.status.code(), manual.status.code());
    assert_eq!(
        normalize_volatile(&stdout_of(&auto)),
        normalize_volatile(&stdout_of(&manual)),
    );
    // Detection announces itself on stderr (suppressed by -q).
    assert!(stderr_of(&auto).contains("detected: test (junit)"));
}

#[test]
fn diff_junit_matches_test_diff_and_exits_1() {
    let dir = tmp_dir("auto-diff");
    let base = dir.join("base.xml");
    let cand = dir.join("cand.xml");
    std::fs::write(&base, JUNIT_XML).unwrap();
    std::fs::write(
        &cand,
        r#"<testsuite tests="1"><testcase name="a" classname="C" time="0"><failure/></testcase></testsuite>"#,
    )
    .unwrap();

    let auto = lens(&["diff", base.to_str().unwrap(), cand.to_str().unwrap()]);
    let manual = lens(&[
        "test",
        "diff",
        base.to_str().unwrap(),
        cand.to_str().unwrap(),
    ]);
    assert_eq!(auto.status.code(), Some(1), "{}", stderr_of(&auto));
    assert_eq!(
        normalize_volatile(&stdout_of(&auto)),
        normalize_volatile(&stdout_of(&manual)),
    );
}

#[test]
fn diff_incompatible_kinds_is_a_usage_error() {
    let dir = tmp_dir("auto-mismatch");
    let junit = dir.join("r.xml");
    std::fs::write(&junit, JUNIT_XML).unwrap();
    let ccdb = dir.join("compile_commands.json");
    std::fs::write(
        &ccdb,
        r#"[{"directory":"/tmp","command":"cc -c a.c","file":"a.c"}]"#,
    )
    .unwrap();

    let out = lens(&["diff", junit.to_str().unwrap(), ccdb.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("cannot diff"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn diff_as_trace_skips_detection() {
    let dir = tmp_dir("auto-as-trace");
    let a = dir.join("a.out");
    let b = dir.join("b.out");
    let trace = "1 openat(AT_FDCWD, \"/x\", O_RDONLY) = 3\n1 close(3) = 0\n";
    std::fs::write(&a, trace).unwrap();
    std::fs::write(&b, trace).unwrap();

    let out = lens(&[
        "diff",
        "--as",
        "trace",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    assert_eq!(json["baseline_calls"], 2, "{json}");
    // --as means no detection happened — nothing to announce.
    assert!(!stderr_of(&out).contains("detected:"));
}

#[test]
fn global_format_flag_works_before_and_after_subcommand() {
    let dir = tmp_dir("global-fmt");
    std::fs::write(dir.join("f.txt"), "x").unwrap();
    let d = dir.to_str().unwrap();
    for args in [
        vec!["--format", "json", "disk", "scan", d],
        vec!["disk", "scan", d, "--format", "json"],
        vec!["disk", "scan", d, "--json"],
    ] {
        let out = lens(&args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr_of(&out));
        let json: serde_json::Value = serde_json::from_str(&stdout_of(&out))
            .unwrap_or_else(|_| panic!("{args:?} did not emit JSON"));
        assert_eq!(json["schema_version"], "diskmap.snapshot/v2", "{args:?}");
    }
}

#[test]
fn legacy_doctor_fail_on_warn_still_parses() {
    let dir = tmp_dir("doctor-legacy");
    let out = lens(&[
        "doctor",
        "--root",
        dir.to_str().unwrap(),
        "--fail-on",
        "warn",
    ]);
    // Parsing is the point; the run itself must not usage-error.
    assert_ne!(out.status.code(), Some(2), "{}", stderr_of(&out));
}

#[test]
fn fail_on_warn_applies_to_every_command() {
    // test parse emits a Warn `test.failures` finding: default threshold
    // (fail) stays 0, --fail-on warn escalates it to 1.
    let dir = tmp_file(
        "fail-on",
        "fail.xml",
        r#"<testsuite tests="1"><testcase name="t"><failure/></testcase></testsuite>"#,
    );
    let f = dir.path.clone();
    let default = lens(&["test", "parse", f.to_str().unwrap()]);
    assert_eq!(default.status.code(), Some(0), "{}", stderr_of(&default));
    let warned = lens(&["test", "parse", f.to_str().unwrap(), "--fail-on", "warn"]);
    assert_eq!(warned.status.code(), Some(1), "{}", stderr_of(&warned));
}

#[test]
fn quiet_suppresses_scan_warnings_but_not_stdout() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp_dir("quiet");
    let locked = dir.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("x"), "x").unwrap();
    std::fs::write(dir.join("ok.txt"), "ok").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let loud = lens(&["disk", "scan", dir.to_str().unwrap()]);
    if !stderr_of(&loud).contains("scan warning") {
        // Running as root bypasses the chmod — nothing to assert.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let quiet = lens(&["-q", "disk", "scan", dir.to_str().unwrap()]);
    assert!(
        !stderr_of(&quiet).contains("scan warning"),
        "{}",
        stderr_of(&quiet)
    );
    // stdout is unaffected by -q.
    assert_eq!(stdout_of(&quiet), stdout_of(&loud));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn user_set_max_depth_is_not_an_error() {
    // Three levels: root/l1/l2/leaf.txt — depth 1 stops before l2.
    let dir = tmp_dir("depth");
    std::fs::create_dir_all(dir.join("l1/l2")).unwrap();
    std::fs::write(dir.join("l1/l2/leaf.txt"), "x").unwrap();

    let out = lens(&["disk", "scan", dir.to_str().unwrap(), "--max-depth", "1"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(
        !stderr_of(&out).contains("scan warning"),
        "{}",
        stderr_of(&out)
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("Scan completed"), "{stdout}");
    assert!(stdout.contains("Depth limit"), "{stdout}");
}

#[test]
fn exclude_exact_name_glob_and_path_forms() {
    let dir = tmp_dir("exclude-forms");
    for d in [".git", ".github", "a/b"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    std::fs::write(dir.join(".github/workflow.yml"), "w").unwrap();
    std::fs::write(dir.join(".gitignore"), "i").unwrap();
    std::fs::write(dir.join("app.log"), "l").unwrap();
    std::fs::write(dir.join("a/b/deep.txt"), "d").unwrap();

    // `.git` is an exact name: it must not eat .github or .gitignore.
    let out = lens(&["disk", "scan", dir.to_str().unwrap(), "--exclude", ".git"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains(".github"), "{stdout}");
    assert!(stdout.contains(".gitignore"), "{stdout}");
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&lens(&[
        "disk",
        "scan",
        dir.to_str().unwrap(),
        "--exclude",
        ".git",
        "--format",
        "json",
    ])))
    .expect("JSON output");
    let names: Vec<&str> = json["root"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect();
    assert!(!names.contains(&".git"), "{names:?}");
    assert!(names.contains(&".github"), "{names:?}");
    assert!(names.contains(&".gitignore"), "{names:?}");

    // `*.log` is a name glob.
    let out = lens(&[
        "disk",
        "scan",
        dir.to_str().unwrap(),
        "--exclude",
        "*.log",
        "--format",
        "json",
    ]);
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    let names: Vec<&str> = json["root"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect();
    assert!(!names.contains(&"app.log"), "{names:?}");

    // `a/b` is a relative-path glob: removes b only under a.
    let out = lens(&[
        "disk",
        "scan",
        dir.to_str().unwrap(),
        "--exclude",
        "a/b",
        "--format",
        "json",
    ]);
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    let a = json["root"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "a")
        .expect("a present");
    assert!(
        a["children"].as_array().unwrap().is_empty(),
        "a/b should be excluded: {a}"
    );
}

#[test]
fn hardlinked_file_counts_once_in_aggregates() {
    let dir = tmp_dir("hardlink");
    let orig = dir.join("orig");
    std::fs::write(&orig, b"12345").unwrap();
    let link = dir.join("link");
    if std::fs::hard_link(&orig, &link).is_err() {
        return; // filesystem without hardlink support — nothing to check
    }

    let out = lens(&["disk", "scan", dir.to_str().unwrap(), "--format", "json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON output");
    // Two 5-byte names for one inode: the aggregate counts the data once.
    assert_eq!(json["root"]["size"], 5, "{json}");
    // The file nodes themselves keep their own sizes.
    let file_sizes: Vec<u64> = json["root"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["size"].as_u64())
        .collect();
    assert_eq!(file_sizes, vec![5, 5], "{json}");
}

#[test]
fn env_diff_accepts_venv_dirs_and_mixed_inputs() {
    let base = make_venv("envdiff-a", &[("requests", "2.31.0", &[])]);
    let cand = make_venv("envdiff-b", &[("requests", "2.32.0", &[])]);

    // dir vs dir
    let out = lens(&[
        "env",
        "diff",
        base.to_str().unwrap(),
        cand.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("version_changes"),
        "{}",
        stdout_of(&out)
    );

    // dir vs snapshot JSON
    let snapdir = tmp_dir("envdiff-snap");
    let snap = snapdir.join("snap.json");
    let inspect = lens(&["env", "inspect", cand.to_str().unwrap(), "--format", "json"]);
    assert!(inspect.status.success(), "{}", stderr_of(&inspect));
    std::fs::write(&snap, stdout_of(&inspect)).unwrap();
    let out = lens(&[
        "env",
        "diff",
        base.to_str().unwrap(),
        snap.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));

    // `lens diff` auto-detects two venvs as env inputs.
    let out = lens(&["diff", base.to_str().unwrap(), cand.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("detected: env"),
        "{}",
        stderr_of(&out)
    );
    assert!(
        stdout_of(&out).contains("version_changes"),
        "{}",
        stdout_of(&out)
    );
}

#[test]
fn disk_duplicates_accepts_scan_options() {
    let dir = tmp_dir("dup-opts");
    let keep = dir.join("keep");
    let gone = dir.join("gone");
    std::fs::create_dir_all(&keep).unwrap();
    std::fs::create_dir_all(&gone).unwrap();
    let blob = vec![9u8; 4096];
    std::fs::write(keep.join("a.bin"), &blob).unwrap();
    std::fs::write(keep.join("b.bin"), &blob).unwrap();
    std::fs::write(gone.join("c.bin"), &blob).unwrap();
    std::fs::write(gone.join("d.bin"), &blob).unwrap();

    let out = lens(&[
        "disk",
        "duplicates",
        dir.to_str().unwrap(),
        "--exclude",
        "gone",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("1 duplicate group"), "{stdout}");
    assert!(stdout.contains("a.bin"), "{stdout}");
    assert!(!stdout.contains("gone"), "{stdout}");
}

// --- Phase 4: snapshot + bundle diff + doctor ---

fn unit_dir(tag: &str, cyclic: bool) -> TestDir {
    let dir = tmp_dir(tag);
    if cyclic {
        std::fs::write(
            dir.join("a.service"),
            "[Unit]\nAfter=b.service\n[Service]\nExecStart=/bin/true\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.service"),
            "[Unit]\nAfter=a.service\n[Service]\nExecStart=/bin/true\n",
        )
        .unwrap();
    } else {
        std::fs::write(
            dir.join("a.service"),
            "[Unit]\n[Service]\nExecStart=/bin/true\n",
        )
        .unwrap();
    }
    dir
}

#[test]
fn snapshot_collects_host_net_sys_doctor_and_verifies() {
    let dir = tmp_dir("snapshot");
    let units = unit_dir("snapshot-units", false);
    let out_path = dir.join("s.lens");

    let out = lens(&[
        "snapshot",
        "-o",
        out_path.to_str().unwrap(),
        "--procfs",
        "/proc",
        "--systemd-dir",
        units.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(out_path.exists());

    let inspect = lens(&[
        "bundle",
        "inspect",
        out_path.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert!(inspect.status.success(), "{}", stderr_of(&inspect));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&inspect)).expect("JSON");
    let names: Vec<&str> = json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    for expected in [
        "reports/host.json",
        "reports/net_report.json",
        "reports/sys_snapshot.json",
        "reports/doctor_report.json",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    let host: serde_json::Value = serde_json::from_str(&stdout_of(&lens(&[
        "bundle",
        "show",
        out_path.to_str().unwrap(),
        "reports/host.json",
    ])))
    .expect("host JSON");
    assert_eq!(host["schema"], "lens.host/v1", "{host}");

    let verify = lens(&["bundle", "verify", out_path.to_str().unwrap()]);
    assert_eq!(verify.status.code(), Some(0), "{}", stderr_of(&verify));
}

#[test]
fn snapshot_default_name_and_log_fallback() {
    let dir = tmp_dir("snapshot-name");
    let cwd = std::env::current_dir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_lens"))
        .args(["snapshot", "--procfs", "/nonexistent-procfs"])
        .current_dir(&*dir)
        .output()
        .expect("spawn lens");
    let _ = cwd;
    // A nonexistent procfs makes net/host collection fail -> the bundle
    // records diagnostics and exits 1, but is still written.
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    let entries: Vec<_> = std::fs::read_dir(&*dir)
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.starts_with("snapshot-").then_some(n)
        })
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(entries[0].ends_with(".lens"), "{}", entries[0]);
}

#[test]
fn bundle_diff_finds_new_cycle_and_regression() {
    let dir = tmp_dir("bundle-diff");
    let clean = unit_dir("bd-clean", false);
    let cyclic = unit_dir("bd-cyclic", true);
    let junit_ok = dir.join("ok.xml");
    let junit_bad = dir.join("bad.xml");
    std::fs::write(&junit_ok, JUNIT_XML).unwrap();
    std::fs::write(
        &junit_bad,
        r#"<testsuite tests="1"><testcase name="a" classname="C" time="0"><failure/></testcase></testsuite>"#,
    )
    .unwrap();

    let a = dir.join("a.lens");
    let b = dir.join("b.lens");
    for (bundle, units, junit) in [(&a, &*clean, &junit_ok), (&b, &*cyclic, &junit_bad)] {
        let out = lens(&[
            "bundle",
            "create",
            bundle.to_str().unwrap(),
            "--sys",
            units.to_str().unwrap(),
            "--test",
            junit.to_str().unwrap(),
        ]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    }

    let out = lens(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("sys:"), "{stdout}");
    assert!(stdout.contains("test:"), "{stdout}");
    assert!(stdout.contains("sys.new_cycle"), "{stdout}");
    assert!(stdout.contains("test.regression"), "{stdout}");
    assert!(
        stderr_of(&out).contains("detected: bundle"),
        "{}",
        stderr_of(&out)
    );

    // Same bundle vs itself: no actionable delta.
    let out = lens(&["bundle", "diff", a.to_str().unwrap(), a.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));

    // JSON exposes the lens.bundle-diff/v1 schema.
    let out = lens(&[
        "bundle",
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--format",
        "json",
    ]);
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON");
    assert_eq!(json["schema"], "lens.bundle-diff/v1", "{json}");
    assert!(json["sections"]["sys"].is_object(), "{json}");
    assert!(json["sections"]["test"].is_object(), "{json}");
}

#[test]
fn bundle_diff_tampered_bundle_exits_2() {
    let dir = tmp_dir("bundle-tamper");
    let a = dir.join("a.lens");
    let out = lens(&[
        "bundle",
        "create",
        a.to_str().unwrap(),
        "--test",
        dir.to_str().unwrap(),
    ]);
    // --test on a dir with no XML is a collection failure but still
    // writes the bundle? If it errors out, create with a real junit.
    if !a.exists() {
        let junit = dir.join("ok.xml");
        std::fs::write(&junit, JUNIT_XML).unwrap();
        let out2 = lens(&[
            "bundle",
            "create",
            a.to_str().unwrap(),
            "--test",
            junit.to_str().unwrap(),
        ]);
        assert_eq!(out2.status.code(), Some(0), "{}", stderr_of(&out2));
    }
    let _ = out;
    // Corrupt bytes mid-archive: verify must fail before any diff.
    let mut bytes = std::fs::read(&a).unwrap();
    let mid = bytes.len() / 2;
    for byte in bytes.iter_mut().skip(mid).take(32) {
        *byte ^= 0xFF;
    }
    let tampered = dir.join("tampered.lens");
    std::fs::write(&tampered, &bytes).unwrap();

    let out = lens(&["diff", a.to_str().unwrap(), tampered.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    let out = lens(&[
        "bundle",
        "diff",
        a.to_str().unwrap(),
        tampered.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
}

// --- Phase 5: log follow/stats/multi-input ---

#[test]
fn log_filter_multiple_inputs_prefixes_and_globs() {
    let dir = tmp_dir("log-multi");
    std::fs::write(dir.join("a.log"), "a1 info\na2 hit\n").unwrap();
    std::fs::write(dir.join("b.log"), "b1 hit\nb2 info\n").unwrap();

    // Two explicit inputs: lines are prefixed [file:line].
    let out = lens(&[
        "log",
        "filter",
        dir.join("a.log").to_str().unwrap(),
        dir.join("b.log").to_str().unwrap(),
        "--query",
        "hit",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains(&format!("[{}:2] a2 hit", dir.join("a.log").display())),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("[{}:1] b1 hit", dir.join("b.log").display())),
        "{stdout}"
    );

    // A filename glob expands (sorted) like the explicit form.
    let out = lens(&[
        "log",
        "filter",
        dir.join("*.log").to_str().unwrap(),
        "--query",
        "hit",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), stdout);

    // A glob matching nothing is a usage error.
    let out = lens(&[
        "log",
        "filter",
        dir.join("*.nope").to_str().unwrap(),
        "--query",
        "hit",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("matched nothing"));

    // A single input keeps the un-prefixed format.
    let out = lens(&[
        "log",
        "filter",
        dir.join("a.log").to_str().unwrap(),
        "--query",
        "hit",
    ]);
    let stdout = stdout_of(&out);
    assert!(stdout.contains("[2] a2 hit"), "{stdout}");
    assert!(!stdout.contains("a.log"), "{stdout}");
}

#[test]
fn log_filter_global_limit_across_inputs() {
    let dir = tmp_dir("log-limit");
    std::fs::write(dir.join("a.log"), "hit1\nhit2\n").unwrap();
    std::fs::write(dir.join("b.log"), "hit3\nhit4\n").unwrap();
    let out = lens(&[
        "log",
        "filter",
        dir.join("a.log").to_str().unwrap(),
        dir.join("b.log").to_str().unwrap(),
        "--query",
        "hit",
        "--limit",
        "3",
        "--format",
        "json",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON");
    assert_eq!(json["matched"], 3, "{json}");
    assert_eq!(json["total"], 4, "{json}");
    assert_eq!(json["inputs"].as_array().unwrap().len(), 2, "{json}");
    // The global limit cut b.log short.
    assert_eq!(json["inputs"][1]["matched"], 1, "{json}");
}

#[test]
fn log_filter_follow_emits_appended_lines() {
    let dir = tmp_dir("log-follow");
    let log_path = dir.join("f.log");
    std::fs::write(&log_path, "seed hit\n").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_lens"))
        .args([
            "log",
            "filter",
            log_path.to_str().unwrap(),
            "--query",
            "hit",
            "--follow",
            "--limit",
            "3",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn lens");

    std::thread::sleep(std::time::Duration::from_millis(700));
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .unwrap();
        writeln!(f, "appended hit one").unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(700));
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .unwrap();
        writeln!(f, "appended hit two").unwrap();
    }

    // --limit 3 should end the follow shortly after the second append.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let status = loop {
        match child.try_wait().unwrap() {
            Some(s) => break s,
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                panic!("follow did not stop after --limit was reached");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    };
    assert!(status.success());
    let mut stdout = String::new();
    use std::io::Read as _;
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert!(stdout.contains("[1] seed hit"), "{stdout}");
    assert!(stdout.contains("[2] appended hit one"), "{stdout}");
    assert!(stdout.contains("[3] appended hit two"), "{stdout}");
}

#[test]
fn log_filter_follow_rejects_bad_combinations() {
    let dir = tmp_dir("log-follow-bad");
    let a = dir.join("a.log");
    let b = dir.join("b.log");
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "x\n").unwrap();

    let out = lens(&[
        "log",
        "filter",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--follow",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_of(&out).contains("exactly one"),
        "{}",
        stderr_of(&out)
    );

    let out = lens(&[
        "log",
        "filter",
        a.to_str().unwrap(),
        "--follow",
        "--context",
        "1",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("--context"), "{}", stderr_of(&out));

    let out = lens(&[
        "log",
        "filter",
        a.to_str().unwrap(),
        "--follow",
        "--format",
        "json",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_of(&out).contains("text or jsonl"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn log_stats_json_schema_and_filter() {
    let dir = tmp_dir("log-stats");
    let log = dir.join("s.log");
    std::fs::write(
        &log,
        "2026-10-09T12:00:01Z INFO ok\n         2026-10-09T12:30:02Z WARN low\n         2026-10-09T12:30:03Z ERROR boom\n         no timestamp here\n",
    )
    .unwrap();

    let out = lens(&["log", "stats", log.to_str().unwrap(), "--format", "json"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON");
    assert_eq!(json["schema"], "lens.log-stats/v1", "{json}");
    assert_eq!(json["total_lines"], 4, "{json}");
    assert_eq!(json["matched_lines"], 4, "{json}");
    assert_eq!(json["by_level"]["error"], 1, "{json}");
    assert_eq!(json["unknown_timestamp"], 1, "{json}");
    assert!(json["buckets"].is_array(), "{json}");

    // A min-level filter scopes the stats.
    let out = lens(&[
        "log",
        "stats",
        log.to_str().unwrap(),
        "--min-level",
        "warn",
        "--bucket",
        "1h",
        "--format",
        "json",
    ]);
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON");
    assert_eq!(json["matched_lines"], 2, "{json}");
    assert_eq!(json["total_lines"], 4, "{json}");
    assert_eq!(json["buckets"].as_array().unwrap().len(), 1, "{json}");

    // Bad bucket string is a usage error.
    let out = lens(&["log", "stats", log.to_str().unwrap(), "--bucket", "2h"]);
    assert_eq!(out.status.code(), Some(2));

    // Text output renders a histogram line.
    let out = lens(&["log", "stats", log.to_str().unwrap()]);
    let stdout = stdout_of(&out);
    assert!(stdout.contains("2026-10-09T12:00:00Z"), "{stdout}");
    assert!(stdout.contains("!2"), "{stdout}"); // 2 warn+ lines in the bucket
}

#[test]
fn log_inspect_multiple_inputs() {
    let dir = tmp_dir("log-inspect");
    std::fs::write(dir.join("a.log"), "x\n").unwrap();
    std::fs::write(dir.join("b.log"), "y\nz\n").unwrap();
    let out = lens(&[
        "log",
        "inspect",
        dir.join("a.log").to_str().unwrap(),
        dir.join("b.log").to_str().unwrap(),
        "--format",
        "json",
    ]);
    let json: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("JSON");
    let arr = json.as_array().expect("array for multi-input");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[1]["total_lines"], 2);
}

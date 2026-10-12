use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// Feed newline-delimited JSON-RPC requests to lens-mcp and parse the
/// response lines. The server exits when stdin reaches EOF.
fn roundtrip(requests: &[&str]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lens-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn lens-mcp");

    {
        let mut stdin = child.stdin.take().unwrap();
        for req in requests {
            writeln!(stdin, "{req}").unwrap();
        }
    }

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let status = child.wait().expect("wait");
    assert!(status.success());

    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("response is JSON"))
        .collect()
}

#[test]
fn ping_returns_empty_result() {
    let responses = roundtrip(&[r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#]);
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["result"], serde_json::json!({}));
    assert!(responses[0].get("error").is_none());
}

#[test]
fn tool_failure_is_an_iserror_result() {
    // lens_disk_scan without the required "path" argument fails inside the
    // tool — the failure must come back as result.isError, not a JSON-RPC
    // error, so the model can read and react to it.
    let responses = roundtrip(&[
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"lens_disk_scan","arguments":{}}}"#,
    ]);
    assert_eq!(responses.len(), 1);
    let resp = &responses[0];
    assert!(resp.get("error").is_none(), "{resp}");
    assert_eq!(resp["result"]["isError"], Value::Bool(true));
    assert_eq!(resp["result"]["content"][0]["type"], "text");
    assert!(resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("path"));
}

#[test]
fn unknown_tool_stays_a_jsonrpc_error() {
    let responses = roundtrip(&[
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"lens_nope","arguments":{}}}"#,
    ]);
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["error"]["code"], Value::from(-32602));
    assert!(responses[0].get("result").is_none());
}

#[test]
fn unknown_method_and_parse_errors_stay_jsonrpc_errors() {
    let responses = roundtrip(&[
        r#"{"jsonrpc":"2.0","id":4,"method":"bogus/method"}"#,
        "this is not json",
    ]);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], Value::from(-32601));
    assert_eq!(responses[1]["error"]["code"], Value::from(-32700));
}

#[test]
fn successful_tool_call_returns_content() {
    let responses = roundtrip(&[
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"lens_disk_scan","arguments":{"path":"/tmp"}}}"#,
    ]);
    assert_eq!(responses.len(), 1);
    let resp = &responses[0];
    assert!(resp.get("error").is_none(), "{resp}");
    assert!(resp["result"]["content"][0]["text"].is_string());
    assert!(resp["result"].get("isError").is_none());
}

fn tool_call(id: u64, name: &str, args: Value) -> String {
    let call = format!(
        r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"name":"{}","arguments":{}}}}}"#,
        id, name, args
    );
    let responses = roundtrip(&[&call]);
    assert_eq!(responses.len(), 1);
    let resp = &responses[0];
    assert!(resp.get("error").is_none(), "{resp}");
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

fn tool_result(id: u64, name: &str, args: Value) -> Value {
    let call = format!(
        r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"name":"{}","arguments":{}}}}}"#,
        id, name, args
    );
    let responses = roundtrip(&[&call]);
    assert_eq!(responses.len(), 1);
    responses[0]["result"].clone()
}

/// Temp dir that is removed on drop, even when the test fails.
struct TestDir {
    _guard: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl std::ops::Deref for TestDir {
    type Target = std::path::PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

fn tmpdir(tag: &str) -> TestDir {
    let guard = tempfile::Builder::new()
        .prefix(&format!("lensmcp-{tag}-"))
        .tempdir()
        .unwrap();
    let path = guard.path().to_path_buf();
    TestDir {
        _guard: guard,
        path,
    }
}

#[test]
fn initialize_echoes_client_protocol_version() {
    let responses = roundtrip(&[
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
    ]);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-03-26");

    // No version in the request → newest supported version.
    let responses = roundtrip(&[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#]);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
}

#[test]
fn initialize_unsupported_protocol_version_gets_latest_supported() {
    let responses = roundtrip(&[
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999"}}"#,
    ]);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
}

#[test]
fn request_without_id_gets_no_response() {
    // A JSON-RPC notification (no id) produces no output line for any
    // method — including tools/call, initialize, and unknown methods.
    let responses = roundtrip(&[
        r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"lens_doctor","arguments":{"root_path":"/"}}}"#,
        r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        r#"{"jsonrpc":"2.0","method":"ping"}"#,
        r#"{"jsonrpc":"2.0","method":"bogus/method"}"#,
        r#"{"jsonrpc":"2.0","method":"tools/list"}"#,
    ]);
    assert!(responses.is_empty(), "notifications must not respond");
}

#[test]
fn log_filter_tail_reaches_end_of_file() {
    // MARKER sits past the old first-1000-lines scan bound — only a tail
    // scan finds it.
    let dir = tmpdir("logtail");
    let log = dir.join("big.log");
    let mut content = String::new();
    for i in 0..5000 {
        content.push_str(&format!("INFO line {}\n", i));
    }
    content.push_str("ERROR MARKER_AT_TAIL\n");
    std::fs::write(&log, content).unwrap();

    let text = tool_call(
        10,
        "lens_log_filter",
        serde_json::json!({
            "path": log.to_string_lossy(),
            "query": "MARKER_AT_TAIL",
            "tail": 100
        }),
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["matches"].as_array().unwrap().len(), 1, "{text}");
    assert_eq!(v["matches"][0]["line"], 5001);
}

#[test]
fn log_filter_limit_paginates_matches() {
    let dir = tmpdir("loglim");
    let log = dir.join("m.log");
    let content: String = (0..50).map(|i| format!("ERROR e{}\n", i)).collect();
    std::fs::write(&log, content).unwrap();

    let text = tool_call(
        11,
        "lens_log_filter",
        serde_json::json!({
            "path": log.to_string_lossy(),
            "query": "ERROR",
            "limit": 10,
            "offset": 5
        }),
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    let matches = v["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 10, "{text}");
    // offset 5 → first emitted match is the 6th ERROR (line 6).
    assert_eq!(matches[0]["line"], 6);
    assert_eq!(v["truncated"], true);
}

#[test]
fn log_filter_since_until_time_window() {
    let dir = tmpdir("logts");
    let log = dir.join("t.log");
    std::fs::write(
        &log,
        "2026-10-09T10:00:00Z INFO early\n\
         2026-10-09T12:00:00Z INFO inside\n\
         Oct  9 13:00:00 host app: syslog\n\
         no timestamp INFO\n",
    )
    .unwrap();

    let text = tool_call(
        11,
        "lens_log_filter",
        serde_json::json!({
            "path": log.to_string_lossy(),
            "since": "2026-10-09T11:00:00Z",
            "until": "2026-10-09T14:00:00Z",
            "year": 2026
        }),
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    let lines: Vec<i64> = v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["line"].as_i64())
        .collect();
    assert_eq!(lines, vec![2, 3], "{text}");

    // Invalid bound → tool error (isError), not a crash.
    let call = format!(
        r#"{{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{{"name":"lens_log_filter","arguments":{{"path":{},"since":"bogus"}}}}}}"#,
        serde_json::to_string(&log.to_string_lossy()).unwrap()
    );
    let responses = roundtrip(&[&call]);
    assert_eq!(responses[0]["result"]["isError"], true);
}

#[test]
fn tool_results_paginate_arrays() {
    // lens_doctor's checks array is capped by `limit`; the cut is
    // reported in top-level `_pagination` metadata — no marker objects
    // are injected into the data itself.
    let text = tool_call(
        12,
        "lens_doctor",
        serde_json::json!({ "root_path": "/", "limit": 1 }),
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    let checks = v["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert!(checks.iter().all(|c| c.get("_truncated").is_none()));
    let page = &v["_pagination"]["checks"];
    assert!(page["total"].as_u64().unwrap() > 0, "{text}");
    assert_eq!(page["returned"], 1);
    assert_eq!(page["offset"], 0);
}

#[test]
fn doctor_bad_override_path_is_iserror() {
    let result = tool_result(
        13,
        "lens_doctor",
        serde_json::json!({ "systemd_dir": "/nonexistent-dir-for-mcp-test" }),
    );
    assert_eq!(result["isError"], Value::Bool(true));
    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("does not exist"));
}

#[test]
fn sys_diff_between_unit_dirs() {
    let base = tmpdir("sysb");
    let cand = tmpdir("sysc");
    let unit = "[Service]\nExecStart=/bin/a\n";
    std::fs::write(base.join("a.service"), unit).unwrap();
    std::fs::write(
        cand.join("a.service"),
        "[Service]\nUser=svc\nExecStart=/bin/a\n",
    )
    .unwrap();

    let text = tool_call(
        14,
        "lens_sys_diff",
        serde_json::json!({
            "baseline": base.to_string_lossy(),
            "candidate": cand.to_string_lossy()
        }),
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["modified_units"][0]["unit"], "a.service", "{text}");
}

#[test]
fn sys_cycles_appends_findings_content_item() {
    // A cyclic result carries the data JSON plus a second content item
    // with the report's findings.
    let dir = tmpdir("cycles");
    std::fs::write(
        dir.join("a.service"),
        "[Unit]\nAfter=b.service\n[Service]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.service"),
        "[Unit]\nAfter=a.service\n[Service]\n",
    )
    .unwrap();

    let result = tool_result(
        16,
        "lens_sys_cycles",
        serde_json::json!({ "dir": dir.to_string_lossy() }),
    );
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{result}");
    let data: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    assert!(!data["cycles"].as_array().unwrap().is_empty());
    let findings: Value = serde_json::from_str(content[1]["text"].as_str().unwrap()).unwrap();
    let list = findings["findings"].as_array().unwrap();
    assert!(
        list.iter()
            .any(|f| f["code"] == "sys.cycle" && f["severity"] == "fail"),
        "{findings}"
    );
}

#[test]
fn test_diff_reports_regressions() {
    let dir = tmpdir("tdiff");
    std::fs::write(
        dir.join("base.xml"),
        r#"<testsuite name="s"><testcase name="a" time="0.1"/></testsuite>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("cand.xml"),
        r#"<testsuite name="s"><testcase name="a" time="0.1"><failure>oops</failure></testcase></testsuite>"#,
    )
    .unwrap();

    let text = tool_call(
        15,
        "lens_test_diff",
        serde_json::json!({
            "baseline": dir.join("base.xml").to_string_lossy(),
            "candidate": dir.join("cand.xml").to_string_lossy()
        }),
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["regressions"].as_array().unwrap().len(), 1, "{text}");
}

#[test]
fn net_diff_between_snapshots() {
    let dir = tmpdir("ndiff");
    let sock = |port: u16| {
        format!(
            r#"{{"kind":"tcp","local_address":"127.0.0.1","local_port":{},"remote_address":"","remote_port":0,"state":"LISTEN","inode":{},"uid":0,"tx_queue":0,"rx_queue":0,"process":null}}"#,
            port, port
        )
    };
    let snap = |listeners: String| {
        format!(
            r#"{{"schema_version":"lens.net/v1","summary":{{"total_sockets":0,"listening_ports":0,"established_connections":0,"time_wait_sockets":0,"orphan_sockets":0,"unix_domain_sockets":0}},"sockets":[],"listening":[{}]}}"#,
            listeners
        )
    };
    let b = dir.join("b.json");
    let c = dir.join("c.json");
    std::fs::write(&b, snap(sock(80))).unwrap();
    std::fs::write(&c, snap(format!("{},{}", sock(80), sock(443)))).unwrap();

    let text = tool_call(
        16,
        "lens_net_diff",
        serde_json::json!({
            "baseline": b.to_string_lossy(),
            "candidate": c.to_string_lossy()
        }),
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["new_listeners"][0]["local_port"], 443, "{text}");
}

#[test]
fn log_filter_counts_lossy_utf8_lines() {
    // A matching line with invalid UTF-8 is still a match (shown lossy),
    // and the scanned lossy lines are counted — not silently dropped.
    let dir = tmpdir("loglossy");
    let log = dir.join("app.log");
    let mut bytes = b"ERROR ok\n".to_vec();
    bytes.extend_from_slice(b"ERROR bad \xff\n");
    bytes.extend_from_slice(b"ERROR ok2\n");
    std::fs::write(&log, bytes).unwrap();

    let text = tool_call(
        18,
        "lens_log_filter",
        serde_json::json!({
            "path": log.to_string_lossy(),
            "query": "error"
        }),
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["matches"].as_array().unwrap().len(), 3, "{text}");
    assert_eq!(v["scanned_lines"], 3, "{text}");
    assert_eq!(v["lossy_lines"], 1, "{text}");
    assert!(v["matches"][1]["raw"]
        .as_str()
        .unwrap()
        .contains('\u{fffd}'));
}

#[test]
fn trace_analyze_rejects_non_strace_input() {
    // The CLI's fail-closed gate applies in MCP too: markdown is an
    // in-band tool error, not a garbage snapshot.
    let dir = tmpdir("tracemd");
    let md = dir.join("notes.md");
    std::fs::write(
        &md,
        "# Notes\n\n- some *markdown* text\n- more lines\n- and more\n- and more\n",
    )
    .unwrap();

    let result = tool_result(
        19,
        "lens_trace_analyze",
        serde_json::json!({ "trace_file": md.to_string_lossy() }),
    );
    assert_eq!(result["isError"], Value::Bool(true), "{result}");
    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("does not look like strace output"));
}

#[test]
fn abi_diff_identical_binary_is_compatible() {
    // The lens-mcp binary itself is an ELF — diffing it against itself is
    // trivially compatible.
    let exe = env!("CARGO_BIN_EXE_lens-mcp");
    let text = tool_call(
        17,
        "lens_abi_diff",
        serde_json::json!({ "baseline": exe, "candidate": exe }),
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["compatibility"], "compatible", "{text}");
}

fn junit_run_json(status: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema": "testlens.run/v1",
        "version": "0.6.1",
        "producer": {"name": "testlens", "version": "0.6.1"},
        "project": "p",
        "run_id": "r",
        "collected_at": "2026-01-01T00:00:00Z",
        "complete": true,
        "cases": [{
            "identity": "C.t1",
            "name": "t1",
            "classname": "C",
            "status": status,
            "duration_sec": 0.0
        }],
        "summary": {"total": 1, "passed": 1, "failed": 0,
                    "errors": 0, "skipped": 0, "duration_sec": 0.0}
    }))
    .unwrap()
}

#[test]
fn lens_bundle_diff_returns_sections_and_findings_item() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |name: &str, status: &str| {
        let p = dir.path().join(name);
        lens_core::create_bundle_archive(
            &p,
            "0.0.0-test",
            &[lens_core::BundleArtifact {
                name: "reports/test_run.json".to_string(),
                data: junit_run_json(status),
            }],
            Vec::new(),
            false,
        )
        .unwrap();
        p
    };
    let a = mk("a.lens", "passed");
    let b = mk("b.lens", "failed");

    let call = format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"lens_bundle_diff","arguments":{{"baseline":{:?},"candidate":{:?}}}}}}}"#,
        a.to_string_lossy(),
        b.to_string_lossy()
    );
    let responses = roundtrip(&[&call]);
    assert_eq!(responses.len(), 1);
    let result = &responses[0]["result"];
    assert!(result.get("isError").is_none(), "{result}");

    // First content item: the bundle-diff JSON with a `test` section.
    let data: serde_json::Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(data["schema"], "lens.bundle-diff/v1", "{data}");
    assert!(data["sections"]["test"].is_object(), "{data}");

    // The test.regression Fail finding appends the findings item.
    let findings: serde_json::Value =
        serde_json::from_str(result["content"][1]["text"].as_str().unwrap()).unwrap();
    let codes: Vec<&str> = findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["code"].as_str())
        .collect();
    assert!(codes.contains(&"test.regression"), "{findings}");
}

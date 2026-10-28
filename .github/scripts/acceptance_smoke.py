#!/usr/bin/env python3
"""Linux real-binary smoke; pass directory containing all three binaries.
Run under the shared product lock during coordinated local acceptance.
Requires Python jsonschema for the versioned consumer corpus check.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time

import jsonschema

repo = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binaries", type=Path)
args = parser.parse_args()
binaries = args.binaries.resolve()
schema = json.loads((repo / "docs/schemas/lens-result-v1.schema.json").read_text())
corpus = repo / "docs/fixtures/result-v1"
for entry in json.loads((corpus / "manifest.json").read_text())["cases"]:
    value = json.loads((corpus / entry["file"]).read_text())
    if entry["kind"] == "envelope":
        jsonschema.validate(value, schema)
    else:
        assert value["exit_code"] == 2 and value["stdout"] == "" and value["stderr"]
print("PASS consumer corpus JSON Schema")

with tempfile.TemporaryDirectory(prefix="lens-acceptance-") as scratch:
    root = Path(scratch)
    def write(name, text):
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        return path
    log = write("app.log", "2026-10-28T01:00:00Z ERROR acceptance fixture\n")
    junit = write("junit.xml", '<testsuite><testcase name="fails" classname="smoke"><failure message="expected fixture failure"/></testcase></testsuite>')
    write("main.c", "int main(void) { return 0; }\n")
    db = write("compile_commands.json", json.dumps([{"directory": str(root), "file": "main.c", "arguments": ["cc", "-c", "main.c"]}]))
    service = write("smoke.service", "[Unit]\nDescription=Smoke\n[Service]\nExecStart=/bin/true\n")
    write("venv/pyvenv.cfg", "home = /usr/bin\nversion = 3.12.0\n")
    (root / "venv/lib/python3.12/site-packages").mkdir(parents=True)
    def report(name, *command):
        result = subprocess.run([str(binaries / "lens"), "--no-config", "--format", "envelope", *map(str, command)], cwd=root, capture_output=True, text=True, timeout=30)
        assert result.returncode in (0, 1), (name, result.returncode, result.stderr)
        value = json.loads(result.stdout)
        jsonschema.validate(value, schema)
        assert value["execution"]["exit_code"] == result.returncode
        print(f"PASS {name}: exit={result.returncode}, complete={value['complete']}")
        return value
    report("disk", "inspect", root)
    report("abi", "inspect", binaries / "lens")
    report("log", "inspect", log)
    report("test", "inspect", junit)
    report("trace", "trace", "analyze", repo / "crates/lens-trace/fixtures/mixed.strace")
    report("sys", "inspect", service)
    report("build", "inspect", db)
    report("env", "inspect", root / "venv")
    report("net (current namespace)", "net", "inspect")
    report("doctor (synthetic root)", "doctor", "--root", root)
    bundle = root / "smoke.lens"
    report("bundle create", "bundle", "create", bundle, "--test", junit, "--log", log)
    report("bundle verify", "bundle", "verify", bundle)
    report("bundle inspect", "bundle", "inspect", bundle)
    report("diff", "diff", bundle, bundle)

    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    child = subprocess.Popen([str(binaries / "lens-tui"), str(root)], stdin=slave, stdout=slave, stderr=slave, env={**os.environ, "TERM": "xterm-256color"}, start_new_session=True)
    os.close(slave)
    captured = bytearray()
    try:
        deadline = time.monotonic() + 10
        sent = False
        while child.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], .1)[0]:
                try:
                    captured.extend(os.read(master, 65536))
                except OSError:
                    break
            if len(captured) > 100 and not sent:
                os.write(master, b"q")
                sent = True
        assert child.wait(timeout=3) == 0, captured[-1000:]
        assert sent and b"\x1b[" in captured
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
        os.close(master)
    print("PASS TUI PTY render and q exit")

    requests = [
        {"id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "acceptance", "version": "1"}}},
        {"id": 2, "method": "tools/list"},
        {"id": 3, "method": "tools/call", "params": {"name": "lens_log_stats", "arguments": {"path": str(repo / "README.md")}}},
        {"id": 4, "method": "tools/call", "params": {"name": "lens_log_stats", "arguments": {"path": str(log)}}},
        {"id": 5, "method": "tools/call", "params": {"name": "lens_doctor", "arguments": {"root_path": str(root), "limit": 1}}},
    ]
    result = subprocess.run([str(binaries / "lens-mcp"), "--allow-root", str(root)], input="".join(json.dumps({"jsonrpc": "2.0", **r}) + "\n" for r in requests), capture_output=True, text=True, timeout=30, check=True)
    responses = {v["id"]: v["result"] for v in map(json.loads, result.stdout.splitlines())}
    assert responses[1]["protocolVersion"] == "2025-03-26"
    assert responses[2]["tools"]
    assert responses[3]["isError"] and "outside the allowed roots" in responses[3]["content"][0]["text"]
    assert not responses[4].get("isError", False)
    doctor = json.loads(responses[5]["content"][0]["text"])
    assert len(doctor["checks"]) == 1
    assert doctor["_pagination"]["checks"]["returned"] == 1
    assert doctor["_pagination"]["checks"]["total"] > 1
    print("PASS MCP initialize/tools/list, allowed-root denial, in-root read, pagination")
print("All acceptance smoke checks passed; temporary fixtures removed")

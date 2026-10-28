#!/usr/bin/env python3
"""Exercise release shell steps with a local fake GitHub API; never publish."""

import hashlib
import io
import tarfile
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())
STEPS = {
    step.get("name"): step.get("run", "")
    for step in WORKFLOW["jobs"]["artifacts"]["steps"]
}
SHA = "a" * 40
TAG = "v0.5.0"
FAKE_GH = r"""#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
args = sys.argv[1:]
path = pathlib.Path(os.environ["MOCK_STATE"])
state = json.loads(path.read_text())
def value(flag):
    return args[args.index(flag) + 1] if flag in args else ""
def finish(event, output=""):
    state["events"].append(event)
    path.write_text(json.dumps(state))
    print(output)
    sys.exit(0)
if args[0] == "api":
    endpoint = next(x for x in args if x.startswith("repos/"))
    fields = dict(x.split("=", 1) for x in args if "=" in x)
    if "/pulls/" in endpoint:
        finish("read-pr", json.dumps({"head": {"repo": {"full_name": os.environ["GH_REPO"]}, "ref": "release-please--branches--main"}}))
    if endpoint.endswith("/git/refs"):
        assert not state["tag"], "ref must never be overwritten"
        state["tag"] = fields["sha"]
        finish("create-tag")
    # The commit-list endpoint repos/.../commits must NOT appear here: it
    # always succeeds and would mask the tag-resolution call. Only the
    # git/ref/tags/<tag> lookup resolves a tag to a commit (404 when absent).
    if "/git/ref/tags/" in endpoint:
        if not state["tag"]:
            print(json.dumps({"message": "Not Found", "status": "404"}), file=sys.stderr)
            sys.exit(1)
        finish("resolve-tag", state["tag"])
if args[:2] == ["workflow", "run"]:
    assert value("--ref") == "release-please--branches--main"
    finish("dispatch-ci")
if args[:2] == ["release", "view"]:
    if not state.get("exists", True): sys.exit(1)
    if value("--json") == "targetCommitish": finish("read-target", state["target"])
    if value("--json") == "isDraft":
        result = state["draft"] if "--jq" in args else {"isDraft": state["draft"]}
        finish("read-state", json.dumps(result))
    finish("view")
if args[:2] == ["release", "create"]:
    assert value("--target") == state["target"]
    state["exists"] = True
    state["draft"] = True
    finish("create-draft")
if args[:2] == ["release", "upload"]: finish("upload")
if args[:2] == ["release", "download"]:
    destination = pathlib.Path(value("--dir"))
    destination.mkdir(parents=True, exist_ok=True)
    for asset in pathlib.Path(os.environ["DIST"]).iterdir():
        shutil.copyfile(asset, destination / asset.name)
    if os.environ.get("MOCK_CORRUPT"):
        next(destination.glob("*.tar.gz")).write_text("corrupt")
    finish("download")
if args[:2] == ["release", "edit"]:
    assert state["tag"] == state["target"]
    state["draft"] = False
    finish("publish")
raise SystemExit("unexpected fake gh command: " + repr(args))
"""


class ReleaseFlow(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="release-flow-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        gh = self.root / "gh"
        gh.write_text(FAKE_GH)
        gh.chmod(0o755)
        self.state_path = self.root / "state.json"
        self.state_path.write_text(
            json.dumps({"tag": None, "draft": True, "target": SHA, "events": []})
        )
        dist = self.root / "dist"
        dist.mkdir()
        content = b"#!/bin/sh\nprintf 'lens 0.5.0\\n'\n"
        names = ("lens", "lens-tui", "lens-mcp")
        build = {"version": "0.5.0", "target": "x86_64-unknown-linux-musl", "source_sha": SHA,
                 "sha256": {name: hashlib.sha256(content).hexdigest() for name in names}}
        archive = dist / f"lens-{TAG}-x86_64-unknown-linux-musl.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            for name, data in [(name, content) for name in names] + [("BUILD.json", json.dumps(build).encode())]:
                member = tarfile.TarInfo(name)
                member.size = len(data)
                member.mode = 0o755
                tar.addfile(member, io.BytesIO(data))
        (dist / "SHA256SUMS.txt").write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + f"  {archive.name}\n")
        self.environment = {
            **os.environ,
            "PATH": str(self.root) + os.pathsep + os.environ["PATH"],
            "MOCK_STATE": str(self.state_path),
            "GITHUB_WORKSPACE": str(ROOT),
            "GH_REPO": "owner/repository",
            "RELEASE_TAG": TAG,
            "REQUESTED_SHA": SHA,
            "SOURCE_SHA": SHA,
            "RUNNER_TEMP": str(self.root),
            "GITHUB_OUTPUT": str(self.root / "outputs"),
            "DIST": str(dist),
            "GH_TOKEN": "mock-token",
        }

    def execute(self, name, expected=0):
        result = subprocess.run(
            ["bash", "-c", STEPS[name]],
            env=self.environment,
            cwd=self.root,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return json.loads(self.state_path.read_text())

    def test_draft_without_tag_uses_exact_commit_and_publishes_last(self):
        self.execute("Resolve the immutable source commit")
        self.assertIn("sha=" + SHA, (self.root / "outputs").read_text())
        state = self.execute("Upload draft assets, verify and publish")
        events = state["events"]
        self.assertLess(events.index("upload"), events.index("download"))
        self.assertLess(events.index("download"), events.index("create-tag"))
        self.assertLess(events.index("create-tag"), events.index("publish"))
        self.assertFalse(state["draft"])

    def test_dispatch_can_recover_draft_source(self):
        self.environment["REQUESTED_SHA"] = ""
        state = self.execute("Resolve the immutable source commit")
        self.assertIn("read-target", state["events"])
        self.assertIn("sha=" + SHA, (self.root / "outputs").read_text())

    def test_existing_different_tag_is_never_moved(self):
        state = json.loads(self.state_path.read_text())
        state["tag"] = "b" * 40
        self.state_path.write_text(json.dumps(state))
        state = self.execute("Resolve the immutable source commit", expected=1)
        self.assertNotIn("create-tag", state["events"])

    def test_bad_download_cannot_tag_or_publish(self):
        self.environment["MOCK_CORRUPT"] = "1"
        state = self.execute("Upload draft assets, verify and publish", expected=1)
        self.assertNotIn("create-tag", state["events"])
        self.assertNotIn("publish", state["events"])

    def test_generated_release_pr_explicitly_dispatches_ci(self):
        document = yaml.safe_load(
            (ROOT / ".github/workflows/release-please.yml").read_text()
        )
        step = next(
            step
            for step in document["jobs"]["release-please"]["steps"]
            if step.get("name") == "Start CI for created or updated release PRs"
        )
        self.environment["RELEASE_PRS"] = json.dumps([{"number": 42}])
        result = subprocess.run(
            ["bash", "-c", step["run"]],
            env=self.environment,
            cwd=self.root,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("dispatch-ci", json.loads(self.state_path.read_text())["events"])

    def test_published_retry_only_verifies_assets(self):
        state = json.loads(self.state_path.read_text())
        state.update(tag=SHA, draft=False)
        self.state_path.write_text(json.dumps(state))
        state = self.execute("Verify an already published release")
        self.assertIn("published=true", (self.root / "outputs").read_text())
        self.assertNotIn("upload", state["events"])
        self.assertNotIn("publish", state["events"])


if __name__ == "__main__":
    unittest.main()

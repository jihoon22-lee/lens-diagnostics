#!/usr/bin/env python3
"""Package only explicit-target release binaries with a source/digest manifest."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile

TARGET = "x86_64-unknown-linux-musl"
root = Path(__file__).resolve().parents[2]
tag = os.environ["RELEASE_TAG"]
sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
if not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
    raise SystemExit("invalid release tag")
binaries = root / "target" / TARGET / "release"
if subprocess.check_output([str(binaries / "lens"), "--version"], text=True).strip() != f"lens {tag[1:]}":
    raise SystemExit("binary version does not match tag")
for name in ("lens", "lens-tui", "lens-mcp"):
    subprocess.run([str(binaries / name), "--help"], check=True, stdout=subprocess.DEVNULL)
    headers = subprocess.check_output(["readelf", "-l", str(binaries / name)], text=True)
    if "INTERP" in headers:
        raise SystemExit(f"{name} unexpectedly requires a dynamic interpreter")
metadata = {
    "version": tag[1:], "target": TARGET, "source_sha": sha,
    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "sha256": {name: hashlib.sha256((binaries / name).read_bytes()).hexdigest() for name in ("lens", "lens-tui", "lens-mcp")},
}
(binaries / "BUILD.json").write_text(json.dumps(metadata, indent=2) + "\n")
dist = root / "dist"
dist.mkdir(exist_ok=True)
if any(dist.iterdir()):
    raise SystemExit("dist must be empty before packaging; refusing stale release artifacts")
archive = dist / f"lens-{tag}-{TARGET}.tar.gz"
with tarfile.open(archive, "w:gz") as output:
    for name in ("lens", "lens-tui", "lens-mcp", "BUILD.json"):
        output.add(binaries / name, arcname=name, recursive=False)
# Preserve the established asset URL for existing installers.
shutil.copyfile(archive, dist / f"lens-{tag}-linux-x86_64.tar.gz")
shutil.copyfile(root / "scripts/install.py", dist / "install.py")
changelog = (root / "CHANGELOG.md").read_text()
section = re.search(r"(?ms)^## " + re.escape(tag[1:]) + r"[^\n]*\n(.*?)(?=^## |\Z)", changelog)
if not section:
    raise SystemExit("CHANGELOG has no release notes for tag")
(dist / "RELEASE_NOTES.md").write_text(f"# Lens Diagnostics {tag}\n\n" + section.group(1).strip() + "\n")
(dist / "SHA256SUMS.txt").write_text("".join(
    f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
    for path in sorted(dist.iterdir()) if path.name != "SHA256SUMS.txt"
))

#!/usr/bin/env python3
"""Verify downloaded release identity and smoke-test its exact executable bytes."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("lens_install", root / "scripts/install.py")
install = importlib.util.module_from_spec(spec)
spec.loader.exec_module(install)
directory, tag, sha = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
name = f"lens-{tag}-{install.TARGET}.tar.gz"
files, build = install.unpack_verified((directory / name).read_bytes(), (directory / "SHA256SUMS.txt").read_bytes(), name, tag)
if build["source_sha"] != sha:
    raise SystemExit("downloaded artifact source SHA differs from release source")
with tempfile.TemporaryDirectory(prefix="lens-release-smoke-") as scratch:
    for name in install.BINARIES:
        path = Path(scratch) / name
        path.write_bytes(files[name])
        path.chmod(0o755)
        subprocess.run([str(path), "--help"], check=True, stdout=subprocess.DEVNULL, timeout=15)
    version = subprocess.check_output([str(Path(scratch) / "lens"), "--version"], text=True, timeout=15).strip()
    if version != f"lens {tag.lstrip('v')}":
        raise SystemExit("downloaded binary version mismatch")
print(f"Verified {tag}: {build['target']}, source {sha}, binary digests and executable smoke passed")

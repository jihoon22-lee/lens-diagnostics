#!/usr/bin/env python3
"""Installer archive validation and offline no-clobber contract."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("lens_install", ROOT / "scripts/install.py")
INSTALL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INSTALL)
TAG = "v0.10.0"
NAME = f"lens-{TAG}-{INSTALL.TARGET}.tar.gz"


def fixture(extra=None):
    content = b"#!/bin/sh\nexit 0\n"
    files = {name: content for name in INSTALL.BINARIES}
    build = {"version": TAG[1:], "target": INSTALL.TARGET, "source_sha": "a" * 40,
             "sha256": {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}}
    files["BUILD.json"] = json.dumps(build).encode()
    if extra:
        files[extra] = b"untrusted"
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w:gz") as archive:
        for name, data in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    data = stream.getvalue()
    return data, (hashlib.sha256(data).hexdigest() + "  " + NAME + "\n").encode()


class Installer(unittest.TestCase):
    def test_digest_version_and_members_are_verified(self):
        archive, sums = fixture()
        files, build = INSTALL.unpack_verified(archive, sums, NAME, TAG)
        self.assertEqual(build["source_sha"], "a" * 40)
        self.assertEqual(set(files), {*INSTALL.BINARIES, "BUILD.json"})
        with self.assertRaises(ValueError):
            INSTALL.unpack_verified(archive + b"corrupt", sums, NAME, TAG)
        with self.assertRaises(ValueError):
            INSTALL.unpack_verified(archive, sums, NAME, "v9.9.9")
        archive, sums = fixture("../escape")
        with self.assertRaises(ValueError):
            INSTALL.unpack_verified(archive, sums, NAME, TAG)

    def test_offline_install_requires_explicit_replacement(self):
        with tempfile.TemporaryDirectory(prefix="lens-installer-test-") as scratch:
            root = Path(scratch)
            archive, sums = fixture()
            (root / NAME).write_bytes(archive)
            (root / "SHA256SUMS.txt").write_bytes(sums)
            args = [sys.executable, str(ROOT / "scripts/install.py"), "--version", TAG,
                    "--archive", str(root / NAME), "--checksums", str(root / "SHA256SUMS.txt"),
                    "--prefix", str(root / "prefix")]
            first = subprocess.run(args, capture_output=True, text=True)
            self.assertEqual(first.returncode, 0, first.stderr)
            lens = root / "prefix/bin/lens"
            self.assertTrue(lens.stat().st_mode & 0o111)
            lens.write_bytes(b"keep existing")
            second = subprocess.run(args, capture_output=True, text=True)
            self.assertNotEqual(second.returncode, 0)
            self.assertEqual(lens.read_bytes(), b"keep existing")
            third = subprocess.run(args + ["--force"], capture_output=True, text=True)
            self.assertEqual(third.returncode, 0, third.stderr)
            self.assertNotEqual(lens.read_bytes(), b"keep existing")


if __name__ == "__main__":
    unittest.main()

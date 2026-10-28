#!/usr/bin/env python3
"""Install verified Lens Diagnostics release executables, including offline archives."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import tarfile
import tempfile
import urllib.request

REPOSITORY = "https://github.com/jihoon22-lee/lens-diagnostics"
TARGET = "x86_64-unknown-linux-musl"
BINARIES = ("lens", "lens-tui", "lens-mcp")
MAX_ARCHIVE = 128 * 1024 * 1024


def read_bounded(path, limit):
    with path.open("rb") as stream:
        value = stream.read(limit + 1)
    if len(value) > limit:
        raise ValueError(f"file exceeds {limit} bytes: {path}")
    return value


def download(url, limit):
    with urllib.request.urlopen(url, timeout=60) as response:
        value = response.read(limit + 1)
    if len(value) > limit:
        raise ValueError("download exceeds size limit")
    return value


def unpack_verified(archive, checksums, filename, version):
    matches = []
    for line in checksums.decode("ascii").splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].lstrip("*") == filename:
            matches.append(parts[0])
    if len(matches) != 1 or hashlib.sha256(archive).hexdigest() != matches[0]:
        raise ValueError("archive SHA-256 does not match the checksum inventory")
    files = {}
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
        for member in tar:
            if member.name not in (*BINARIES, "BUILD.json") or not member.isfile() or member.name in files:
                raise ValueError(f"unexpected/duplicate archive member: {member.name}")
            if member.size > MAX_ARCHIVE or sum(map(len, files.values())) + member.size > MAX_ARCHIVE:
                raise ValueError("unpacked release exceeds size limit")
            files[member.name] = tar.extractfile(member).read()
    if set(files) != {*BINARIES, "BUILD.json"}:
        raise ValueError("release is missing required members")
    build = json.loads(files["BUILD.json"])
    if build.get("target") != TARGET or build.get("version") != version.lstrip("v"):
        raise ValueError("release target/version does not match request")
    if not re.fullmatch(r"[0-9a-f]{40}", build.get("source_sha", "")):
        raise ValueError("release source SHA is missing")
    for name in BINARIES:
        if hashlib.sha256(files[name]).hexdigest() != build["sha256"][name]:
            raise ValueError(f"binary digest mismatch: {name}")
    return files, build


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="Exact release, e.g. v0.10.0")
    parser.add_argument("--prefix", type=Path, default=Path.home() / ".local")
    parser.add_argument("--archive", type=Path, help="Offline target archive (requires --checksums)")
    parser.add_argument("--checksums", type=Path, help="Offline SHA256SUMS.txt")
    parser.add_argument("--force", action="store_true", help="Replace existing regular executables")
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() not in ("x86_64", "AMD64"):
        parser.error("this release installer supports Linux x86_64; other platforms need a supported source build")
    if not re.fullmatch(r"v?\d+\.\d+\.\d+", args.version):
        parser.error("--version must be an exact stable vX.Y.Z")
    if bool(args.archive) != bool(args.checksums):
        parser.error("--archive and --checksums must be supplied together")
    tag = "v" + args.version.lstrip("v")
    filename = f"lens-{tag}-{TARGET}.tar.gz"
    if args.archive:
        if args.archive.name != filename:
            parser.error(f"offline archive must be named {filename}")
        archive = read_bounded(args.archive, MAX_ARCHIVE)
        sums = read_bounded(args.checksums, 64 * 1024)
    else:
        base = f"{REPOSITORY}/releases/download/{tag}"
        archive = download(f"{base}/{filename}", MAX_ARCHIVE)
        sums = download(f"{base}/SHA256SUMS.txt", 64 * 1024)
    files, build = unpack_verified(archive, sums, filename, tag)
    destination = args.prefix / "bin"
    destination.mkdir(parents=True, exist_ok=True)
    for name in BINARIES:
        path = destination / name
        if path.is_symlink() or (path.exists() and (not args.force or not path.is_file())):
            raise ValueError(f"refusing to replace {path}; --force only replaces regular files")
    # Stage each verified binary in its destination filesystem before publishing.
    for name in BINARIES:
        with tempfile.NamedTemporaryFile(prefix=f".{name}-", dir=destination, delete=False) as stream:
            temporary = Path(stream.name)
            try:
                stream.write(files[name])
                stream.flush()
                os.fchmod(stream.fileno(), 0o755)
                if args.force:
                    os.replace(temporary, destination / name)
                else:
                    os.link(temporary, destination / name)  # no-clobber even across races
            finally:
                temporary.unlink(missing_ok=True)
    print(f"Installed Lens {tag} ({TARGET}, source {build['source_sha']}) in {destination}")
    print(f"Ensure {destination} is on PATH.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, tarfile.TarError) as error:
        raise SystemExit(f"install failed: {error}")

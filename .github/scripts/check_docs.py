#!/usr/bin/env python3
"""Check tracked documentation links, schema references and product version ownership.

This complements the review of prose and executable examples; it does not infer
that a documented feature works merely because a link or schema is valid.
"""

import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from urllib.parse import unquote, urlsplit

from jsonschema.validators import validator_for

ROOT = Path(__file__).resolve().parents[2]


def tracked_files() -> list[Path]:
    result = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT)
    return [ROOT / name.decode() for name in result.split(b"\0") if name]


def local_links(document: Path, errors: list[str]) -> None:
    # Examples may contain illustrative Markdown. Only rendered prose links
    # participate in the repository link contract.
    prose = re.sub(r"(?ms)^\s*(```|~~~).*?^\s*\1\s*$", "", document.read_text())
    links = re.findall(r"\[[^\]]*\]\((?:<([^>]+)>|([^\s)]+))(?:\s+\"[^\"]*\")?\)", prose)
    references = re.findall(r"(?m)^\s*\[[^\]]+\]:\s*(\S+)", prose)
    for href in [first or second for first, second in links] + references:
        parts = urlsplit(href)
        if parts.scheme or parts.netloc or not parts.path or href.startswith("/"):
            continue
        target = (document.parent / unquote(parts.path)).resolve()
        if not target.is_relative_to(ROOT) or not target.exists():
            errors.append(f"{document.relative_to(ROOT)}: missing local link {href}")
    if document.name == "README.md" or document.parent.name == "docs":
        if re.search(r"not (?:yet )?published|current development version|development checkpoint", prose, re.I):
            errors.append(f"{document.relative_to(ROOT)}: stale development/release status")


def schema_references(value: object, root: dict, path: Path, errors: list[str]) -> None:
    if isinstance(value, dict):
        reference = value.get("$ref")
        if isinstance(reference, str) and reference.startswith("#/"):
            current = root
            try:
                for part in reference[2:].split("/"):
                    current = current[unquote(part).replace("~1", "/").replace("~0", "~")]
            except (KeyError, TypeError):
                errors.append(f"{path.relative_to(ROOT)}: unresolved schema reference {reference}")
        for child in value.values():
            schema_references(child, root, path, errors)
    elif isinstance(value, list):
        for child in value:
            schema_references(child, root, path, errors)


def match_version(path: str, pattern: str) -> str:
    match = re.search(pattern, (ROOT / path).read_text(), re.I)
    if not match:
        raise ValueError(f"cannot read version from {path}")
    return match[1]


def versions(errors: list[str]) -> None:
    """The workspace ships a single version; keep Cargo.toml, the release
    manifest, and CHANGELOG in agreement."""
    manifest = json.loads((ROOT / ".release-please-manifest.json").read_text())
    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text())
    version = cargo["workspace"]["package"]["version"]
    key = "." if "." in manifest else next(iter(manifest), None)
    manifest_version = manifest.get(".") or manifest.get(key or "", "")
    if manifest_version and manifest_version != version:
        errors.append(
            f"workspace version {version} differs from release manifest {manifest_version}"
        )
    changelog = (ROOT / "CHANGELOG.md").read_text()
    if not re.search(rf"^## (?:\[)?{re.escape(version)}(?:\]|\s|$)", changelog, re.M):
        errors.append(f"CHANGELOG has no entry for {version}")


def main() -> int:
    errors: list[str] = []
    paths = tracked_files()
    documents = [path for path in paths if path.suffix == ".md"]
    schemas = [path for path in paths if path.name.endswith(".schema.json")]
    for path in documents:
        local_links(path, errors)
    for path in schemas:
        try:
            schema = json.loads(path.read_text())
            validator_for(schema).check_schema(schema)
            schema_references(schema, schema, path, errors)
        except Exception as error:
            errors.append(f"{path.relative_to(ROOT)}: {error}")
    result_schema = ROOT / "docs/schemas/lens-result-v1.schema.json"
    result_fixture = ROOT / "docs/fixtures/lens-result-v1.json"
    if result_schema.exists() and result_fixture.exists():
        try:
            schema = json.loads(result_schema.read_text())
            validator_for(schema)(schema).validate(json.loads(result_fixture.read_text()))
        except Exception as error:
            errors.append(f"Lens result fixture: {error}")
    versions(errors)
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        return 1
    print(f"Documentation: {len(documents)} Markdown files, {len(schemas)} schemas, "
          "workspace version check passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Build a bounded documentation tree and rewrite repository-relative links."""
from pathlib import Path
import argparse
import os
import re
import shutil
from urllib.parse import quote, urlsplit

ROOT = Path(__file__).resolve().parents[2]
CRATES_DIR = ROOT / 'crates'
PRODUCTS = tuple(f"crates/{p.name}" for p in sorted(CRATES_DIR.iterdir()) if p.is_dir()) if CRATES_DIR.is_dir() else ()


def assemble(destination: Path) -> None:
    destination = destination.resolve()
    if destination == ROOT or destination in ROOT.parents:
        raise ValueError('documentation output cannot replace repository sources')
    destination.mkdir(parents=True, exist_ok=True)
    copies: dict[Path, Path] = {}
    for folder in (ROOT, *(ROOT / product for product in PRODUCTS)):
        for name in ('README.md', 'CHANGELOG.md', 'ROADMAP.md', 'LICENSE'):
            source = folder / name
            if source.is_file():
                relative = source.relative_to(ROOT)
                if name == 'README.md':
                    relative = relative.with_name('index.md')
                copies[source] = destination / relative
        for name in ('docs', 'schemas', 'examples', 'assets', 'resources'):
            tree = folder / name
            if tree.is_dir():
                for source in tree.rglob('*'):
                    if source.is_file() and not source.is_symlink() and source.stat().st_size <= 4 * 1024 * 1024:
                        copies[source] = destination / source.relative_to(ROOT)
    # crates/ itself is not a product directory, but its consolidated README
    # is the canonical crate index for the site.
    consolidated = CRATES_DIR / 'README.md'
    if consolidated.is_file():
        copies[consolidated] = destination / 'crates' / 'index.md'
    repository = os.environ.get('GITHUB_REPOSITORY', 'jihoon22-lee/lens-diagnostics')
    revision = os.environ.get('GITHUB_SHA', 'main')
    for source, target in copies.items():
        target.parent.mkdir(parents=True, exist_ok=True)
        if source.suffix != '.md':
            shutil.copyfile(source, target)
            continue
        def link(match: re.Match[str]) -> str:
            label, href = match.groups()
            parts = urlsplit(href)
            if parts.scheme or parts.netloc or not parts.path or href.startswith('/'):
                return match.group(0)
            referenced = (source.parent / parts.path).resolve()
            if referenced.is_dir():
                referenced /= 'README.md'
            if referenced in copies:
                output = copies[referenced]
                if output.suffix == '.md':
                    output = output.with_suffix('.html')
                relative = os.path.relpath(output, target.parent)
                return f'[{label}]({relative}' + (f'#{parts.fragment}' if parts.fragment else '') + ')'
            if referenced.is_relative_to(ROOT) and referenced.exists():
                path = quote(referenced.relative_to(ROOT).as_posix())
                return f'[{label}](https://github.com/{repository}/blob/{revision}/{path}' + (f'#{parts.fragment}' if parts.fragment else '') + ')'
            return match.group(0)
        text = re.sub(r'\[([^\]]*)\]\(([^\s)]+)\)', link, source.read_text())
        target.write_text(text)
    (destination / '_config.yml').write_text('title: Lens Diagnostics\ndescription: Evidence-based Linux and artifact diagnostics\ntheme: jekyll-theme-cayman\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('destination', type=Path)
    assemble(parser.parse_args().destination)

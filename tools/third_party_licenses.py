#!/usr/bin/env python3
"""Generate THIRD_PARTY_LICENSES.md from the locked dependency graphs.

Sources (all lock-file pinned, output sorted and deterministic):

* Rust: ``cargo metadata --locked`` for the three Cargo workspaces
  (repository root, ``desktop/src-tauri``, ``tools/ink-renderer``). The listing
  is the whole lock-file universe for every target platform, i.e. a superset
  of what any single binary links. ``tools/ink-renderer`` needs the pinned
  sdocx checkout at ``references/upstream/sdocx`` (see NOTICE).
* npm: ``desktop/package-lock.json`` (licence from the lock entry, else from
  the installed ``node_modules/<pkg>/package.json``).

    python3 tools/third_party_licenses.py            # write THIRD_PARTY_LICENSES.md
    python3 tools/third_party_licenses.py --check    # CI: fail if stale
    python3 tools/third_party_licenses.py --offline  # use only the local cargo cache
    python3 tools/third_party_licenses.py --texts DIR
        # also copy every dependency's LICENSE/COPYING/NOTICE files into DIR
        # (for release bundles; needs the crate sources in the cargo cache and
        # an installed desktop/node_modules)

Exit status 2 means a dependency has no declared licence or one that is not
in the allow list below (keep it in sync with deny.toml).
"""
from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / 'THIRD_PARTY_LICENSES.md'
CARGO_WORKSPACES = (
    ('Rust engine workspace', 'Cargo.toml'),
    ('Tauri desktop shell', 'desktop/src-tauri/Cargo.toml'),
    ('ink-render CLI (GPL-3.0, links sdocx)', 'tools/ink-renderer/Cargo.toml'),
    ('Live pen preview WebAssembly (GPL-3.0, links sdocx)', 'crates/notes-ink-live/Cargo.toml'),
)
NPM_LOCK = ROOT / 'desktop/package-lock.json'
# SPDX ids accepted anywhere in an expression (mirrors deny.toml).
ALLOWED = {
    'MIT', 'MIT-0', 'Apache-2.0', 'LLVM-exception', 'BSD-2-Clause', 'BSD-3-Clause', 'ISC',
    'Zlib', '0BSD', 'CC0-1.0', 'Unlicense', 'Unicode-3.0', 'MPL-2.0',
    'GPL-3.0', 'GPL-3.0-only', 'GPL-3.0-or-later', 'LGPL-2.1-or-later',
}
LICENSE_FILE = re.compile(r'^(licen[cs]e|copying|notice|copyright|unlicense|authors)([-_.].*)?$', re.I)


def normalize(expr: str) -> str:
    """Old cargo style ``MIT/Apache-2.0`` -> SPDX ``MIT OR Apache-2.0``."""
    expr = re.sub(r'\s*/\s*', ' OR ', expr.strip())
    return re.sub(r'\s+', ' ', expr)


def acceptable(expr: str) -> bool:
    """True if some OR-branch consists only of allowed ids."""
    if not expr:
        return False
    stripped = expr.replace('(', ' ').replace(')', ' ')
    # Evaluate OR of ANDs (WITH binds to its license id).
    for branch in re.split(r'\bOR\b', stripped):
        ids = [t for t in re.split(r'\bAND\b|\bWITH\b', branch) if t.strip()]
        if ids and all(t.strip() in ALLOWED for t in ids):
            return True
    return False


def cargo_packages(manifest: Path, offline: bool) -> tuple[list[dict], set[str]]:
    cmd = ['cargo', 'metadata', '--format-version', '1', '--locked', '--manifest-path', str(manifest)]
    if offline:
        cmd.append('--offline')
    proc = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit(f'cargo metadata failed for {manifest.relative_to(ROOT)}:\n{proc.stderr.strip()}')
    data = json.loads(proc.stdout)
    return data['packages'], set(data['workspace_members'])


def rust_rows(offline: bool):
    sections = []
    for title, rel in CARGO_WORKSPACES:
        manifest = ROOT / rel
        packages, members = cargo_packages(manifest, offline)
        rows = []
        for pkg in packages:
            if pkg['id'] in members:
                continue
            local = pkg.get('source') is None
            pkg_dir = Path(pkg['manifest_path']).resolve().parent
            if local and pkg_dir.is_relative_to(ROOT) and not pkg_dir.is_relative_to(ROOT / 'references'):
                continue  # first-party crate of another workspace in this repository
            lic = pkg.get('license') or ''
            rows.append({
                'name': pkg['name'],
                'version': pkg['version'],
                'license': normalize(lic) if lic else '',
                'repository': pkg.get('repository') or pkg.get('homepage') or '',
                'origin': 'path dependency' if local else 'crates.io',
                'dir': str(Path(pkg['manifest_path']).parent),
            })
        rows.sort(key=lambda r: (r['name'], r['version']))
        sections.append((title, rel, rows))
    return sections


def npm_rows():
    lock = json.loads(NPM_LOCK.read_text(encoding='utf-8'))
    rows = []
    for path, pkg in lock.get('packages', {}).items():
        if not path:
            continue
        name = pkg.get('name') or path.split('node_modules/')[-1]
        lic = pkg.get('license')
        installed = NPM_LOCK.parent / path / 'package.json'
        meta = {}
        if installed.is_file():
            meta = json.loads(installed.read_text(encoding='utf-8'))
        if lic is None:
            lic = meta.get('license')
        if isinstance(lic, dict):
            lic = lic.get('type')
        repo = meta.get('repository') or ''
        if isinstance(repo, dict):
            repo = repo.get('url', '')
        scope = 'dev' if pkg.get('dev') else 'runtime (bundled into the UI)'
        if pkg.get('optional'):
            scope += ', optional/platform'
        rows.append({
            'name': name, 'version': pkg.get('version', ''), 'license': normalize(lic or ''),
            'repository': '', 'origin': scope, 'dir': str(NPM_LOCK.parent / path),
        })
    rows.sort(key=lambda r: (r['name'], r['version']))
    return rows


def table(rows) -> list[str]:
    lines = ['| Package | Version | License | Origin |', '|---|---|---|---|']
    for r in rows:
        lines.append(f"| {r['name']} | {r['version']} | {r['license'] or '**none declared**'} | {r['origin']} |")
    return lines


def summary(rows) -> list[str]:
    counts = Counter(r['license'] or 'none declared' for r in rows)
    return [f'- {lic}: {n}' for lic, n in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))]


def branding() -> dict:
    try:
        return json.loads((ROOT / 'branding.json').read_text(encoding='utf-8'))
    except (OSError, ValueError):
        return {}


def render(rust, npm) -> tuple[str, list[str]]:
    problems = []
    brand = branding()
    name = brand.get('appName', 'This project')
    out = [
        f'# {name}: third-party licenses',
        '',
        '<!-- Generated by tools/third_party_licenses.py; do not edit by hand. -->',
        '',
        f'{name} ({brand.get("repository", "")}) is distributed under the GNU',
        'General Public License version 3 (see LICENSE and NOTICE). It builds on the third-party',
        'packages below. Each listing is generated from a lock file and covers',
        'every platform in that lock file, so it is a superset of what a given',
        'binary contains. Release bundles ship the license texts collected with',
        '`tools/third_party_licenses.py --texts`.',
        '',
        'System libraries linked or executed at runtime are not vendored: OpenSSL 3',
        '(Apache-2.0), SQLite (public domain), zlib, WebKitGTK/GTK (LGPL-2.1+),',
        'Poppler utilities (GPL-2.0-or-later, separate program), librsvg',
        '(LGPL-2.1+, separate program), ImageMagick (ImageMagick license,',
        'separate program), bubblewrap (LGPL-2.0+, separate program).',
        '',
    ]
    for title, rel, rows in rust:
        out += [f'## Rust: {title}', '', f'Lock file: `{Path(rel).parent / "Cargo.lock"}` ({len(rows)} packages)', '']
        out += summary(rows) + [''] + table(rows) + ['']
        problems += [f"{rel}: {r['name']} {r['version']} ({r['license'] or 'none'})" for r in rows if not acceptable(r['license'])]
    out += ['## npm: desktop UI', '', f'Lock file: `desktop/package-lock.json` ({len(npm)} packages)', '']
    out += summary(npm) + [''] + table(npm) + ['']
    problems += [f"npm: {r['name']} {r['version']} ({r['license'] or 'none'})" for r in npm if not acceptable(r['license'])]
    out += [
        '## Icons',
        '',
        'The UI icons in `desktop/assets-free/windows-assets/` are rendered from',
        'Lucide (ISC; portions MIT from Feather). See `desktop/assets-free/LICENSE`.',
        '',
        '## Fonts',
        '',
        'The page text editor uses Roboto (Apache-2.0) in `desktop/public/page-fonts/`, the same files',
        'the pinned sdocx crate bundles. See `desktop/public/page-fonts/Roboto-LICENSE.txt`.',
        '',
    ]
    return '\n'.join(out), problems


def copy_texts(rust, npm, dest: Path) -> int:
    copied = 0
    dest.mkdir(parents=True, exist_ok=True)
    seen = set()
    groups = [rows for _, _, rows in rust] + [npm]
    for rows in groups:
        for r in rows:
            key = (r['name'], r['version'])
            if key in seen:
                continue
            seen.add(key)
            src = Path(r['dir'])
            if not src.is_dir():
                continue
            files = [p for p in sorted(src.iterdir()) if p.is_file() and LICENSE_FILE.match(p.name)]
            if not files:
                continue
            target = dest / f"{r['name'].replace('/', '__')}-{r['version']}"
            target.mkdir(parents=True, exist_ok=True)
            for f in files:
                shutil.copyfile(f, target / f.name)
                copied += 1
    return copied


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--check', action='store_true', help='fail if THIRD_PARTY_LICENSES.md is stale')
    parser.add_argument('--offline', action='store_true', help='pass --offline to cargo metadata')
    parser.add_argument('--texts', type=Path, help='copy license files of every dependency into this directory')
    parser.add_argument('--output', type=Path, default=OUT)
    args = parser.parse_args()

    sdocx = ROOT / 'references/upstream/sdocx'
    if not sdocx.is_dir():
        print('references/upstream/sdocx is missing; fetch sdocx at the pinned commit first (see NOTICE).', file=sys.stderr)
        return 1
    rust = rust_rows(args.offline)
    npm = npm_rows()
    text, problems = render(rust, npm)
    if args.texts:
        print(f'copied {copy_texts(rust, npm, args.texts)} license files into {args.texts}')
    if problems:
        print('dependencies with missing or non-allowed licenses:', file=sys.stderr)
        for p in problems:
            print(f'  {p}', file=sys.stderr)
    if args.check:
        current = args.output.read_text(encoding='utf-8') if args.output.exists() else ''
        if current != text:
            print(f'{args.output.name} is stale; run python3 tools/third_party_licenses.py', file=sys.stderr)
            return 1
    else:
        args.output.write_text(text, encoding='utf-8')
        print(f'wrote {args.output.relative_to(ROOT) if args.output.is_relative_to(ROOT) else args.output}')
    return 2 if problems else 0


if __name__ == '__main__':
    raise SystemExit(main())

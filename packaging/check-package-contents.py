#!/usr/bin/env python3
"""Inspect a built package and fail on content that must not ship.

    packaging/check-package-contents.py PACKAGE [PACKAGE...]

PACKAGE is a .deb, .rpm, .AppImage, .pkg.tar.zst, .flatpak bundle is NOT
supported (pass the flatpak-builder build directory or an exported
`files/` tree instead), or a plain directory.

Checks (all packages):
  * Samsung UI assets, by content: every icon under a `windows-assets/`
    directory must be byte-identical to the free replacement set
    (desktop/assets-free/windows-assets, Lucide/ISC); any file whose sha256
    is a known Samsung asset (desktop/windows-assets-provenance.json when
    present, or NOTES_SAMSUNG_ASSET_HASHES=file of hashes) is flagged
    anywhere; Segoe/Samsung font files are flagged by name. (The UI keeps
    the `windows-assets/` path; the public tree overlays the free icons.)
  * Python: no `samsung_notes/` package, no `.py` / `.pyc` files, no
    libpython, no PyO3 extension modules (`*.so` exporting `PyInit_`), no
    python dependency in the package metadata (deb Depends, rpm Requires,
    pacman .PKGINFO depend).
Prints a short report (file count, unpacked size, findings). Exit 1 on a
finding unless NOTES_ALLOW_SAMSUNG_ASSETS=1 (asset findings only; local test
builds while the replacement assets are pending). Exit 3 when a needed
unpacking tool is missing (bsdtar or dpkg-deb, unsquashfs).
"""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

FONT_PATH = re.compile(r'(^|/)(segoe|segui|samsung)[^/]*\.(ttf|otf|woff2?)$', re.IGNORECASE)
ICON_DIR = re.compile(r'(^|/)windows-assets/[^/]+$')
ROOT = Path(__file__).resolve().parent.parent
PYTHON_PATH = re.compile(r'\.(py|pyc|pyo)$|(^|/)samsung_notes(/|$)|(^|/)libpython3[^/]*\.so|\.cpython-[^/]*\.so$|\.abi3\.so$')
PYTHON_DEP = re.compile(r'(^|[\s,|(])python(3)?([\s,|)<>=]|-numpy|$)', re.IGNORECASE)
SCAN_LIMIT = 256 * 1024 * 1024


def sha256(path: Path) -> str:
    import hashlib
    return hashlib.sha256(path.read_bytes()).hexdigest()


def free_hashes() -> set[str]:
    base = ROOT / 'desktop/assets-free/windows-assets'
    return {sha256(f) for f in base.rglob('*') if f.is_file()} if base.is_dir() else set()


def samsung_hashes() -> set[str]:
    import json
    found: set[str] = set()
    prov = ROOT / 'desktop/windows-assets-provenance.json'
    if prov.is_file():
        found |= {i['sha256'].lower() for i in json.loads(prov.read_text()) if isinstance(i, dict) and i.get('sha256')}
    extra = os.environ.get('NOTES_SAMSUNG_ASSET_HASHES')
    if extra and Path(extra).is_file():
        found |= set(re.findall(r'\b[0-9a-f]{64}\b', Path(extra).read_text().lower()))
    return found


class MissingTool(Exception):
    pass


def run(*args: str, **kw) -> subprocess.CompletedProcess:
    return subprocess.run(list(args), check=True, **kw)


def extract(package: Path, dest: Path) -> str:
    """Unpack `package` into `dest`; return its dependency metadata text."""
    name = package.name
    meta = ''
    if package.is_dir():
        shutil.copytree(package, dest, symlinks=True, dirs_exist_ok=True)
        return meta
    if name.endswith('.deb'):
        if shutil.which('dpkg-deb'):
            run('dpkg-deb', '-x', str(package), str(dest))
            meta = run('dpkg-deb', '-f', str(package), 'Depends', 'Recommends', capture_output=True, text=True).stdout
        elif shutil.which('bsdtar'):
            with tempfile.TemporaryDirectory() as outer:
                run('bsdtar', '-xf', str(package), '-C', outer)
                data = next(Path(outer).glob('data.tar*'))
                control = next(Path(outer).glob('control.tar*'))
                run('bsdtar', '-xf', str(data), '-C', str(dest))
                cdir = Path(outer) / 'control'
                cdir.mkdir()
                run('bsdtar', '-xf', str(control), '-C', str(cdir))
                text = (cdir / 'control').read_text(errors='replace')
                meta = '\n'.join(l for l in text.splitlines() if l.startswith(('Depends:', 'Recommends:')))
        else:
            raise MissingTool('dpkg-deb or bsdtar')
        return meta
    if name.endswith('.rpm') or '.pkg.tar' in name:
        if not shutil.which('bsdtar'):
            raise MissingTool('bsdtar')
        run('bsdtar', '-xf', str(package), '-C', str(dest))
        if name.endswith('.rpm') and shutil.which('rpm'):
            meta = run('rpm', '-qp', '--requires', str(package), capture_output=True, text=True).stdout
        info = dest / '.PKGINFO'
        if info.is_file():
            meta = '\n'.join(l for l in info.read_text().splitlines() if l.startswith(('depend', 'optdepend')))
        return meta
    if name.endswith('.AppImage'):
        offset = run(str(package.resolve()), '--appimage-offset', capture_output=True, text=True,
                     env={**os.environ, 'APPIMAGE_EXTRACT_AND_RUN': ''}).stdout.strip()
        if shutil.which('unsquashfs'):
            run('unsquashfs', '-q', '-n', '-f', '-o', offset, '-d', str(dest), str(package), stdout=subprocess.DEVNULL)
        else:
            raise MissingTool('unsquashfs')
        return meta
    raise SystemExit(f'unsupported package type: {package}')


def scan(package: Path) -> int:
    allow_assets = os.environ.get('NOTES_ALLOW_SAMSUNG_ASSETS') == '1'
    with tempfile.TemporaryDirectory(prefix='notes-pkgcheck-') as tmp:
        root = Path(tmp) / 'root'
        root.mkdir()
        try:
            meta = extract(package, root)
        except MissingTool as missing:
            print(f'{package.name}: SKIPPED content check (needs {missing})')
            return 3
        files, size = 0, 0
        assets: list[str] = []
        python: list[str] = []
        free, samsung = free_hashes(), samsung_hashes()
        icons = 0
        for path in sorted(root.rglob('*')):
            if path.is_symlink() or not path.is_file():
                continue
            rel = path.relative_to(root).as_posix()
            files += 1
            size += path.stat().st_size
            digest = sha256(path) if path.stat().st_size <= SCAN_LIMIT else ''
            if FONT_PATH.search(rel):
                assets.append(f'{rel} (Segoe/Samsung font)')
            elif digest in samsung:
                assets.append(f'{rel} (known Samsung asset hash)')
            elif ICON_DIR.search(rel):
                icons += 1
                if digest not in free:
                    assets.append(f'{rel} (not in the free icon set)')
            elif PYTHON_PATH.search(rel):
                python.append(rel)
            elif digest and path.suffix.lower() not in ('.md', '.txt'):
                data = path.read_bytes()
                if data[:4] == b'\x7fELF' and (b'libpython3' in data or b'PyInit_' in data):
                    python.append(f'{rel} (libpython / PyO3 extension module)')
        python_deps = [l.strip() for l in meta.splitlines() if PYTHON_DEP.search(l)]
        print(f'{package.name}: {files} files, {size / 1e6:.1f} MB unpacked')
        if meta.strip():
            print('  dependencies: ' + ' '.join(meta.split()))
        status = 0
        if python or python_deps:
            status = 1
            for item in python + python_deps:
                print(f'  PYTHON: {item}')
        else:
            print('  python: none (no samsung_notes/, .py, libpython, PyO3 .so or python dependency)')
        if assets:
            label = 'SAMSUNG ASSETS (allowed for this local build)' if allow_assets else 'SAMSUNG ASSETS'
            reasons: dict[str, list[str]] = {}
            for a in assets:
                name, reason = a.rsplit(' (', 1)
                reasons.setdefault(reason.rstrip(')'), []).append(name)
            for reason, names in reasons.items():
                print(f'  {label}: {len(names)} file(s), {reason}: e.g. {names[0]}')
            if not allow_assets:
                status = 1
        else:
            print(f'  samsung assets: none ({icons} UI icons, all from the free set; '
                  f'{len(samsung)} known Samsung hashes checked)')
        return status


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    worst = 0
    for arg in sys.argv[1:]:
        worst = max(worst, scan(Path(arg)))
    return worst


if __name__ == '__main__':
    raise SystemExit(main())

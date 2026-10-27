#!/usr/bin/env python3
"""Offline npm sources for the Flatpak build, straight from package-lock.json.

    packaging/flatpak/npm_sources.py desktop/package-lock.json -o packaging/flatpak/node-sources.json

Every locked package becomes a flatpak-builder `file` source (registry
tarball URL + the lockfile's sha512 integrity) under
`flatpak-node/tarballs/`. The manifest adds them to an empty npm cache with
`npm cache add` and then runs `npm ci --offline`, which resolves everything
from the lockfile, so no registry metadata (packuments) is needed and nothing
is downloaded here. Platform-specific optional packages are kept only for
Linux glibc on x86_64/aarch64 (the Flatpak arches).
"""
from __future__ import annotations

import argparse
import base64
import json
import re
from pathlib import Path

ARCHES = {'x64', 'arm64'}


def wanted(meta: dict) -> bool:
    os_ = meta.get('os')
    cpu = meta.get('cpu')
    libc = meta.get('libc')
    if os_ and 'linux' not in os_:
        return False
    if cpu and not ARCHES.intersection(cpu):
        return False
    if libc and 'glibc' not in libc:
        return False
    return True


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('lockfile', type=Path)
    parser.add_argument('-o', '--output', type=Path, required=True)
    args = parser.parse_args()
    lock = json.loads(args.lockfile.read_text())
    if lock.get('lockfileVersion') not in (2, 3):
        raise SystemExit('need lockfileVersion 2 or 3')
    sources, seen = [], set()
    for key, meta in sorted(lock['packages'].items()):
        if not key or meta.get('link') or not wanted(meta):
            continue
        url, integrity = meta.get('resolved'), meta.get('integrity', '')
        if not url or not integrity.startswith('sha512-'):
            raise SystemExit(f'{key}: missing resolved URL or sha512 integrity')
        digest = base64.b64decode(integrity[len('sha512-'):]).hex()
        if digest in seen:
            continue
        seen.add(digest)
        name = re.sub(r'[^A-Za-z0-9._-]', '_', key.split('node_modules/')[-1]) + '-' + meta['version'] + '.tgz'
        sources.append({'type': 'file', 'url': url, 'sha512': digest,
                        'dest': 'flatpak-node/tarballs', 'dest-filename': name})
    args.output.write_text(json.dumps(sources, indent=4) + '\n')
    print(f'{args.output}: {len(sources)} npm tarballs')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

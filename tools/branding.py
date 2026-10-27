#!/usr/bin/env python3
"""Single source of truth for the product name / application id.

The product name is not decided yet. Every packaging file that needs the
name, the reverse-DNS application id or the binary name is generated from
``branding.json`` at the repository root, so renaming the app is a one-line
change there followed by ``python3 tools/branding.py sync``.

Templates live next to their outputs as ``*.in`` files and use ``@KEY@``
placeholders (``@APP_NAME@``, ``@APP_ID@``, ``@BINARY_NAME@``,
``@REPOSITORY@``, ``@SUMMARY@``, ``@SUMMARY_TR@``, ``@APP_NAME_TR@``,
``@DISCLAIMER@``, ``@LICENSE@``,
``@DEVELOPER_NAME@``). A template whose file name contains ``@APP_ID@``
produces an output named after the current id.

    python3 tools/branding.py get appId          # print one value
    python3 tools/branding.py sync               # regenerate all outputs
    python3 tools/branding.py sync --check       # CI: fail if outputs are stale
    python3 tools/branding.py tauri-config OUT   # Tauri --config overlay JSON
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / 'branding.json'
# Directories scanned for *.in templates.
TEMPLATE_DIRS = ('packaging/flatpak', 'packaging/linux', 'packaging/appimage')
REQUIRED = ('appName', 'appNameTr', 'appId', 'binaryName', 'repository', 'summary', 'summaryTr', 'disclaimer', 'license', 'developerName')
APP_ID = re.compile(r'^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_-]*){2,}$')


def load() -> dict[str, str]:
    data = json.loads(CONFIG.read_text(encoding='utf-8'))
    missing = [k for k in REQUIRED if not isinstance(data.get(k), str) or not data[k]]
    if missing:
        raise SystemExit(f'branding.json: missing {", ".join(missing)}')
    if not APP_ID.match(data['appId']):
        raise SystemExit('branding.json: appId must be reverse-DNS (a.b.c)')
    if not re.fullmatch(r'[a-z0-9][a-z0-9-]*', data['binaryName']):
        raise SystemExit('branding.json: binaryName must be lowercase [a-z0-9-]')
    return data


def placeholders(data: dict[str, str]) -> dict[str, str]:
    def key(name: str) -> str:
        return re.sub(r'(?<!^)(?=[A-Z])', '_', name).upper()
    return {f'@{key(k)}@': v for k, v in data.items() if isinstance(v, str)}


def render(text: str, data: dict[str, str]) -> str:
    for token, value in placeholders(data).items():
        text = text.replace(token, value)
    left = re.findall(r'@[A-Z_]+@', text)
    if left:
        raise SystemExit(f'unknown placeholder(s): {sorted(set(left))}')
    return text


def templates() -> list[Path]:
    found: list[Path] = []
    for rel in TEMPLATE_DIRS:
        base = ROOT / rel
        if base.is_dir():
            found.extend(sorted(p for p in base.rglob('*.in') if p.is_file()))
    return found


def output_for(template: Path, data: dict[str, str]) -> Path:
    name = template.name[:-3]
    return template.with_name(name.replace('@APP_ID@', data['appId']))


def sync(check: bool) -> int:
    data = load()
    stale = []
    for template in templates():
        out = output_for(template, data)
        text = render(template.read_text(encoding='utf-8'), data)
        if out.exists() and out.read_text(encoding='utf-8') == text:
            continue
        stale.append(out.relative_to(ROOT))
        if not check:
            out.write_text(text, encoding='utf-8')
    if check and stale:
        print('branding outputs are stale; run python3 tools/branding.py sync:', file=sys.stderr)
        for path in stale:
            print(f'  {path}', file=sys.stderr)
        return 1
    for path in stale:
        print(f'wrote {path}')
    return 0


def tauri_config(out: Path) -> None:
    data = load()
    overlay = {
        'productName': data['appName'],
        'identifier': data['appId'],
        'mainBinaryName': data['binaryName'],
    }
    out.write_text(json.dumps(overlay, indent=2) + '\n', encoding='utf-8')


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='cmd', required=True)
    get = sub.add_parser('get')
    get.add_argument('key', choices=REQUIRED)
    s = sub.add_parser('sync')
    s.add_argument('--check', action='store_true')
    t = sub.add_parser('tauri-config')
    t.add_argument('out', type=Path)
    args = parser.parse_args()
    if args.cmd == 'get':
        print(load()[args.key])
        return 0
    if args.cmd == 'sync':
        return sync(args.check)
    tauri_config(args.out)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

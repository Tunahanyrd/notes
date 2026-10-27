#!/usr/bin/env bash
# Private source export for a trusted collaborator (e.g. the macOS tester).
# Local only: writes into OUT_DIR, never pushes or uploads anything.
#
#   tools/export_for_collaborator.sh OUT_DIR [--dry-run] [--git] [--strict]
#                                    [--forbid TEXT]... [--name NAME]
#
#   --dry-run   only print what would be included/excluded (counts and sizes)
#   --git       also create OUT_DIR/repo: a fresh git repository with one
#               commit by a placeholder author (no history from this tree)
#   --strict    exit 1 when the privacy scan has blocking findings
#   --forbid    extra private text that must not appear (account e-mail, a
#               real note title, a device serial); repeatable. Also read from
#               NOTES_EXPORT_FORBID (comma separated). Values are never printed.
#
# Left out: artifacts/ (Android/Windows packages, raw analysis dumps that may
# hold account-derived data), references/ (fetch sdocx with
# tools/fetch_sdocx.sh or the commands in docs/MACOS_PORT.md), backups/,
# tools/vendor/, every *.sqlite3*, desktop/dist*, node_modules, target dirs,
# built extensions, internal hand-off notes (CLAUDE.md, WORK.md, RESEARCH.md,
# docs/RESUME_*), private UI screenshots, live Cloud probes (tools/live_*.py),
# fonts, and the Samsung UI icons (desktop/public/windows-assets, replaced by
# the free Lucide set from desktop/assets-free). Output: OUT_DIR/report.json,
# OUT_DIR/MANIFEST.txt, OUT_DIR/<name>.tar.gz (and OUT_DIR/repo with --git).
set -euo pipefail
if [ "$#" -lt 1 ] || [ "${1#-}" != "$1" ]; then
  sed -n '2,24p' "$0" >&2
  exit 2
fi
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec python3 -I - "$root" "$@" <<'PY'
import hashlib, json, os, re, shutil, subprocess, sys, tarfile, getpass
from pathlib import Path

root = Path(sys.argv[1]); args = sys.argv[2:]
out = Path(args.pop(0)).resolve()
dry = git = strict = False; forbid = []; name = 'notes-collaborator-src'
while args:
    a = args.pop(0)
    if a == '--dry-run': dry = True
    elif a == '--git': git = True
    elif a == '--strict': strict = True
    elif a == '--forbid': forbid.append(args.pop(0))
    elif a == '--name': name = args.pop(0)
    else: sys.exit(f'unknown option: {a}')
forbid += [v for v in os.environ.get('NOTES_EXPORT_FORBID', '').split(',') if v.strip()]
if out == root or root in out.parents:
    sys.exit('OUT_DIR must be outside the source tree')

TOP_FILES = {'Cargo.toml', 'Cargo.lock', 'README.md', 'LICENSE', 'NOTICE', 'branding.json', 'deny.toml',
             '.gitignore', '.dockerignore', 'Dockerfile', 'THIRD_PARTY_LICENSES.md', 'THIRD_PARTY_LICENSES.json',
             'rust-toolchain.toml', 'CONTRIBUTING.md', 'SECURITY.md', 'CODE_OF_CONDUCT.md'}
TOP_DIRS = {'crates', 'desktop', 'tools', 'docs', 'packaging', 'bin', '.github'}
PRUNE_ANYWHERE = {'.git', 'target', 'node_modules', '__pycache__', '.venv', '.pytest_cache', 'test-results',
                  '.flatpak-builder', '.mypy_cache'}
INTERNAL_NOTES = {'CLAUDE.md', 'WORK.md', 'RESEARCH.md', 'design-qa.md', 'desktop/design-qa.md',
                  'crates/RUST_PORT_BRIEF.md'}
FONT_EXT = {'.ttf', '.otf', '.ttc', '.woff', '.woff2'}
BUILT_EXT = {'.so', '.dylib', '.pyc', '.tsbuildinfo', '.o', '.a', '.rlib'}

# Same file policy as the public export (tools/public_export.py is the source
# of truth): a file the public tree would not contain is not exported here either.
import importlib.util as _ilu
_spec = _ilu.spec_from_file_location('public_export', root / 'tools/public_export.py')
public_export = _ilu.module_from_spec(_spec); _spec.loader.exec_module(public_export)

def reason(rel: str, is_dir: bool):
    if not is_dir and not public_export.public_allowed(rel):
        if rel.startswith('desktop/public/windows-assets/'):
            return 'Samsung UI icons (replaced by desktop/assets-free)'
        return 'outside the public export policy (tools/public_export.py)'
    parts = rel.split('/')
    top = parts[0]
    if parts[-1] in PRUNE_ANYWHERE and is_dir: return 'build/cache dirs (target, node_modules, __pycache__, ...)'
    if top == 'artifacts':
        if is_dir and len(parts) == 1: return None   # classify one level down
        sub = parts[1] if len(parts) > 1 else ''
        if sub == 'android': return 'artifacts/android (Samsung APKs)'
        if sub.startswith('windows'): return 'artifacts/windows* (Samsung Windows package)'
        if sub == 'analysis': return 'artifacts/analysis (raw research dumps)'
        return 'artifacts/ other'
    if top == 'references': return 'references/ (fetch sdocx at the pinned commit instead)'
    if top == 'reference': return 'reference/python (historical Python spec and tests; not exported)'
    if top == 'backups': return 'backups/'
    if rel == 'tools/vendor' or rel.startswith('tools/vendor/'): return 'tools/vendor (third-party runtimes)'
    if re.match(r'desktop/dist($|-|/)', rel): return 'desktop/dist* (build output)'
    if rel == 'desktop/public/windows-assets' or rel.startswith('desktop/public/windows-assets/'):
        return 'Samsung UI icons (replaced by desktop/assets-free)'
    if rel == 'desktop/windows-assets-provenance.json': return 'Samsung UI icons (replaced by desktop/assets-free)'
    if rel == 'desktop/tests/artifacts' or rel.startswith('desktop/tests/artifacts/'): return 'private UI screenshots'
    if is_dir:
        if len(parts) == 1 and top not in TOP_DIRS: return 'not in allowlist (top level)'
        return None
    if len(parts) == 1 and top not in TOP_FILES: return 'internal notes' if rel in INTERNAL_NOTES else 'not in allowlist (top level)'
    if rel in INTERNAL_NOTES or re.match(r'docs/RESUME_.*\.md$', rel): return 'internal notes'
    if re.search(r'\.sqlite3(-wal|-shm)?$', rel): return 'SQLite databases'
    if Path(rel).suffix.lower() in FONT_EXT: return 'font files'
    if Path(rel).suffix in BUILT_EXT): return 'built binaries/extensions'
    if re.match(r'tools/live_[^/]*\.py$', rel): return 'live Cloud probes (tools/live_*.py)'
    return None

included = []   # (rel, size)
excluded = {}   # reason -> [files, bytes]
def add_excluded(why, path: Path):
    files = size = 0
    if path.is_dir() and not path.is_symlink():
        for dp, dn, fn in os.walk(path):
            for f in fn:
                p = Path(dp) / f
                try: size += p.lstat().st_size; files += 1
                except OSError: pass
    else:
        files, size = 1, path.lstat().st_size
    e = excluded.setdefault(why, [0, 0]); e[0] += files; e[1] += size

for dp, dn, fn in os.walk(root):
    d = Path(dp); rel_d = '' if d == root else d.relative_to(root).as_posix()
    keep = []
    for x in sorted(dn):
        rel = f'{rel_d}/{x}' if rel_d else x
        why = reason(rel, True)
        if why: add_excluded(why, d / x)
        elif (d / x).is_symlink(): add_excluded('symlinks', d / x)
        else: keep.append(x)
    dn[:] = keep
    for f in sorted(fn):
        rel = f'{rel_d}/{f}' if rel_d else f
        p = d / f
        why = reason(rel, False)
        if why: add_excluded(why, p); continue
        if p.is_symlink() or not p.is_file(): add_excluded('symlinks/special files', p); continue
        included.append((rel, p.stat().st_size))

overlay = root / 'desktop/assets-free/windows-assets'
overlay_files = sorted(p for p in overlay.glob('*') if p.is_file()) if overlay.is_dir() else []

# ---------------------------------------------------------------- privacy scan
samsung_hashes = set()
prov = root / 'desktop/windows-assets-provenance.json'
if prov.is_file():
    samsung_hashes |= {e.get('sha256') for e in json.loads(prov.read_text()) if isinstance(e, dict)}
wa = root / 'desktop/public/windows-assets'
if wa.is_dir():
    samsung_hashes |= {hashlib.sha256(p.read_bytes()).hexdigest() for p in wa.glob('*') if p.is_file()}
samsung_hashes -= {hashlib.sha256(p.read_bytes()).hexdigest() for p in overlay_files}
user = getpass.getuser()
PATTERNS = {
    'bearer token': (re.compile(rb'Bearer [A-Za-z0-9._~+/-]{24,}'), True),
    'JWT-like token': (re.compile(rb'eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.'), True),
    'signed URL': (re.compile(rb'(X-Amz-Signature|X-Goog-Signature|[?&]Signature=)[A-Za-z0-9%]{16,}'), True),
    f'private home path (/home/{user}/)': (re.compile(re.escape(f'/home/{user}/').encode()), False),
}
findings = {}   # label -> {'blocking': bool, 'files': [...]}
def hit(label, rel, blocking):
    f = findings.setdefault(label, {'blocking': blocking, 'files': []}); f['files'].append(rel)
for rel, size in included:
    data = (root / rel).read_bytes()
    if data.startswith(b'SQLite format 3\0'): hit('SQLite database content', rel, True)
    if data[:4] in (b'\x7fELF', b'\xcf\xfa\xed\xfe', b'\xca\xfe\xba\xbe'): hit('native executable', rel, True)
    if hashlib.sha256(data).hexdigest() in samsung_hashes: hit('Samsung UI asset (by hash)', rel, True)
    if size > 8 * 1024 * 1024: hit('file larger than 8 MiB', rel, False)
    for label, (rx, blocking) in PATTERNS.items():
        if rx.search(data): hit(label, rel, blocking)
    for i, term in enumerate(forbid):
        if term and term.encode() in data: hit(f'--forbid value #{i + 1}', rel, True)

def mib(n): return f'{n / 1048576:.1f} MiB'
inc_bytes = sum(s for _, s in included) + sum(p.stat().st_size for p in overlay_files)
by_top = {}
for rel, size in included:
    top = rel.split('/')[0] if '/' in rel else '(top-level files)'
    e = by_top.setdefault(top, [0, 0]); e[0] += 1; e[1] += size
blocking = {k: v for k, v in findings.items() if v['blocking']}
report = {
    'included': {'files': len(included) + len(overlay_files), 'bytes': inc_bytes,
                 'byTopLevel': {k: {'files': v[0], 'bytes': v[1]} for k, v in sorted(by_top.items())},
                 'overlay': {'desktop/public/windows-assets <- desktop/assets-free/windows-assets': len(overlay_files)}},
    'excluded': {k: {'files': v[0], 'bytes': v[1]} for k, v in sorted(excluded.items())},
    'scan': {k: {'blocking': v['blocking'], 'count': len(v['files']), 'files': v['files'][:50]} for k, v in sorted(findings.items())},
    'forbidValuesChecked': len(forbid),
}
print(f"included: {report['included']['files']} files, {mib(inc_bytes)}")
for k, v in sorted(by_top.items()): print(f'  {k:<22} {v[0]:>6} files {mib(v[1]):>10}')
print(f'  overlay free icons     {len(overlay_files):>6} files')
print('excluded:')
for k, v in sorted(excluded.items(), key=lambda kv: -kv[1][1]): print(f'  {k:<60} {v[0]:>7} files {mib(v[1]):>10}')
print('privacy scan:' + ('' if findings else ' no findings'))
for k, v in sorted(findings.items()): print(f"  {'BLOCKING' if v['blocking'] else 'review  '} {k}: {len(v['files'])} files")
if dry:
    print('dry run: nothing written'); sys.exit(1 if (strict and blocking) else 0)
if strict and blocking:
    sys.exit('blocking findings; nothing written (see above)')

tree = out / 'tree'
if tree.exists(): shutil.rmtree(tree)
tree.mkdir(parents=True)
for rel, _ in included:
    dst = tree / rel; dst.parent.mkdir(parents=True, exist_ok=True); shutil.copy2(root / rel, dst)
dst_wa = tree / 'desktop/public/windows-assets'; dst_wa.mkdir(parents=True, exist_ok=True)
for p in overlay_files: shutil.copy2(p, dst_wa / p.name)
(out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
manifest = sorted(p.relative_to(tree).as_posix() for p in tree.rglob('*') if p.is_file())
(out / 'MANIFEST.txt').write_text(''.join(f'{(tree / m).stat().st_size}\t{m}\n' for m in manifest))
archive = out / f'{name}.tar.gz'
def norm(ti):
    ti.uid = ti.gid = 0; ti.uname = ti.gname = ''; ti.mtime = 0; return ti
with tarfile.open(archive, 'w:gz') as tf:
    for m in manifest: tf.add(tree / m, arcname=f'{name}/{m}', filter=norm)
print(f'wrote {archive} ({mib(archive.stat().st_size)}), {out / "MANIFEST.txt"}, {out / "report.json"}')
if git:
    repo = out / 'repo'
    if repo.exists(): shutil.rmtree(repo)
    shutil.copytree(tree, repo)
    env = {**os.environ, 'GIT_AUTHOR_DATE': '2026-01-01T00:00:00Z', 'GIT_COMMITTER_DATE': '2026-01-01T00:00:00Z'}
    ident = ['-c', 'user.name=Collaborator Export', '-c', 'user.email=collaborator-export@invalid', '-c', 'commit.gpgsign=false']
    subprocess.run(['git', 'init', '-q', '-b', 'main', str(repo)], check=True)
    subprocess.run(['git', '-C', str(repo), 'add', '-A'], check=True)
    subprocess.run(['git', *ident, '-C', str(repo), 'commit', '-q', '-m', 'Source snapshot for the macOS port'], check=True, env=env)
    print(f'wrote {repo} (fresh repository, 1 commit, no remote)')
PY

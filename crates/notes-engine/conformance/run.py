#!/usr/bin/env python3
"""HTTP conformance harness: reference engine vs candidate engine, byte for byte.

Usage
-----
    # Product gate (no Python package, Python 3 stdlib only): compare the pure
    # engine against the frozen golden fixtures in conformance/golden/.
    cargo build -p notes-engine
    python3 crates/notes-engine/conformance/run.py --replay --candidate target/debug/notes-engine

    # Live comparison against another engine (historical: the Python reference,
    # frozen 2026-10-27 under reference/python; not product):
    python3 reference/python/conformance/gen_library.py --out LIB
    python3 crates/notes-engine/conformance/run.py --library LIB \\
        --candidate "target/debug/notes-engine" [--reference "PYTHONPATH=reference/python NOTES_IMPL=rust python3 -m samsung_notes"] \\
        [--report out.json] [--scratch DIR] [--keep] [--scenario main,uninit] [--repeat N] [-v]

    # Freeze a reference into golden fixtures (reference only, no candidate):
    python3 crates/notes-engine/conformance/run.py --library LIB --record [DIR] --reference CMD

Golden fixtures (``--record`` / ``--replay``)
---------------------------------------------
``golden/`` holds the synthetic input library (``library/``, from
gen_library.py; synthetic data only, never a user library), one
``<scenario>.json.gz`` per scenario with every reference response (status,
headers, body) and the reference's masked final SQLite/file state, and
``golden.json`` (reference command, date, tools present while recording).
The recording root is stored as a same-length neutral stand-in, and every
server root is padded to ``ROOT_LENGTH`` characters, so Content-Length stays
comparable across machines. ``--replay`` starts only the candidate, rebuilds
the reference side from the recording and applies the same masking as a live
run (per-side wall-clock windows, roots, ports, learned id mapping). The
full recording was made with every optional tool present. ``--record
--hide-tools magick --variant no-magick`` stores a variant recorded with those
tools hidden from the servers' PATH (a symlink farm); replay picks the variant
whose hidden set equals the tools missing locally (Ubuntu 24.04 has no
ImageMagick 7 ``magick``), else the full recording. ``golden.json`` also keeps
the renderer versions (pdftoppm, rsvg-convert, magick, ink-render sha256); when
one differs locally, PNG responses are compared by status, headers (except
Content-Length) and IHDR geometry and rendered files (``*.png``, the exported
PDF) by path and mode only (``PASS~``, counted as loose-equal); everything
else stays byte/masked-exact.
The goldens were recorded from the Python reference after a live 1968/1968
PASS. An intended behaviour change of the Rust engine is re-blessed with
``--record --reference target/debug/notes-engine`` (review the new goldens).
When the harness step list changes, replay refuses and asks to re-record.

``--candidate`` / ``--reference`` are command prefixes (shell-split; leading
``NAME=value`` words become environment variables). The harness appends
``--offline --no-auto-sync --data-dir COPY --port 0 --runtime-file F
--no-browser --frontend-dir DIR``. Commands run from the repository root.

What happens per scenario (one per data directory in LIB, see manifest.json):

* the scenario directory is copied twice (``srvA`` = reference, ``srvB`` =
  candidate; roots padded to ROOT_LENGTH so paths in responses keep their length);
* each server gets its own private scratch HOME, XDG_STATE_HOME (with a
  synthetic ``samsung-notes-research/ui-reference/fonts`` + manifest),
  XDG_CONFIG_HOME (``user-dirs.dirs`` -> ``$HOME/Downloads``), TMPDIR and a
  synthetic frontend directory (index.html, js, css, svg, json, nested files,
  inside/outside symlinks). Nothing under ~/.local/state is ever read; no
  network is used (``--offline``);
* both are started, the runtime file is awaited, and the SAME request sequence
  is sent to both over raw sockets (exact header order/casing, malformed
  requests included). Status line, headers (only ``Date`` ignored) and body are
  compared byte for byte;
* responses that differ only by generated values are compared after masking:
  fresh UUIDs (numbered by first appearance; candidate ids learned from
  responses are mapped to reference ids), current wall-clock timestamps
  (ms/us/s within the run window), the per-server scratch root and port;
* previews/thumbnails that answer ``202 pending`` are polled until final;
* after both servers stop, a masked dump of every SQLite table (rows sorted)
  and every file written under the data dirs and Downloads (path, mode,
  sha256; PDF dates normalised) are compared.

Output: one PASS/FAIL line per request (``-v`` also prints passes), a compact
diff for each failure, totals (``N/M byte-equal``, ``K masked-equal``) and an
optional JSON report. Exit status 0 only when everything passed.

Self-check: ``--candidate`` equal to the reference must give 100 % (run it a
few times; ``--repeat N`` does that in one call).

Prerequisites for full coverage (otherwise only error paths are compared):
``pdfinfo`` (POST /pdf, /template), ``bwrap`` + ``pdftoppm`` (PDF background
previews), ``magick`` (export-file pdf), ``xdg-user-dir`` (Downloads lookup),
``tools/ink-renderer/target/{release,debug}/ink-render`` (ink-reconstructed
previews/thumbnails). Python 3 stdlib only (this script never imports the
historical ``samsung_notes`` package).
"""
import argparse
import atexit
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import socket
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[3]
GOLDEN = Path(__file__).resolve().parent / 'golden'
# Historical Python reference (frozen under reference/python, not product).
DEFAULT_REFERENCE = 'PYTHONPATH=reference/python NOTES_IMPL=rust python3 -m samsung_notes'
# Every server root string has exactly this length, so paths inside responses
# keep their byte length (Content-Length) across servers, runs and machines.
ROOT_LENGTH = 160
# Same-length neutral stand-in stored in goldens instead of the recording root.
GOLDEN_ROOT = '/golden-root/'
# samsung_notes.shortcuts.DEFAULT_BINDINGS (schema 2) as of the 2026-10-27 freeze.
DEFAULT_SHORTCUTS = {
    'newNote': 'Ctrl+N', 'searchNotes': 'Ctrl+K', 'saveNote': 'Ctrl+S', 'zoomIn': 'Ctrl+Plus', 'zoomOut': 'Ctrl+Minus',
    'readingMode': 'Alt+Enter', 'fullScreen': 'Alt+F', 'penSettings': 'Ctrl+T', 'pageSettings': 'Alt+P',
    'printNote': 'Ctrl+Shift+P', 'bold': 'Ctrl+B', 'italic': 'Ctrl+I', 'underline': 'Ctrl+U',
    'strikethrough': 'Ctrl+Slash', 'alignLeft': 'Ctrl+L', 'alignCenter': 'Ctrl+E', 'alignRight': 'Ctrl+R',
    'numberedList': 'Ctrl+Shift+7', 'bulletList': 'Ctrl+Shift+8', 'checklist': 'Ctrl+Shift+C',
    'fontSmaller': 'Ctrl+BracketLeft', 'fontLarger': 'Ctrl+BracketRight', 'indent': 'Ctrl+M', 'outdent': 'Ctrl+Shift+M',
}
UUID_RE = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}')
SOCKET_TIMEOUT = 40
PROCESSES = []
# Run-wide settings: 'path' = PATH given to the servers (a farm without the
# hidden tools when --hide-tools is used); 'loose' = compare renderer output
# (PNG bytes, the exported PDF) by geometry/mode only because a renderer tool
# version differs from the recording environment (replay only).
HARNESS = {'path': None, 'loose': False}
RENDER_TOOLS = ('pdftoppm', 'rsvg-convert', 'magick', 'ink-render')


# ---------------------------------------------------------------------------
# Environment: scratch roots, frontend, fonts
# ---------------------------------------------------------------------------

FRONTEND = {
    'index.html': b'<!doctype html><meta charset="utf-8"><title>Notlar</title><div id="root">Sentetik \xc3\xb6n y\xc3\xbcz</div>\n',
    'assets/app.js': b'console.log("synthetic \xe2\x9c\x93");\n',
    'assets/app.css': b'body{font-family:"Segoe UI",sans-serif}\n',
    'icons/logo.svg': b'<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>\n',
    'assets/nested/deep/data.json': b'{"synthetic": true, "t\xc3\xbcrk\xc3\xa7e": "\xc5\x9f"}\n',
    'robots.txt': b'User-agent: *\n',
    'file.weird': b'\x00\x01binary',
    'fonts/x.woff2': b'wOF2synthetic',
    'manifest.webmanifest': b'{"name": "Notlar"}',
    'Boşluk dosya.txt': 'Unicode adı ✓\n'.encode(),
    'assets/map.js.map': b'{"version":3}',
}
FONT_VALID = b'\x00\x01\x00\x00' + b'synthetic-font-table-' * 8


def make_server_root(root, library_dir, empty_dirs=()):
    """Private per-server scratch: data copy, HOME, XDG dirs, frontend, fonts."""
    root.mkdir(parents=True, mode=0o700)
    data = root / 'data'
    shutil.copytree(library_dir, data, symlinks=True,
                    ignore=shutil.ignore_patterns('*.sqlite3-wal', '*.sqlite3-shm', '*.sqlite3-journal'))
    # A git checkout of the golden library has neither the generator's private
    # modes nor its empty directories: restore both (no-op for a fresh library).
    for rel in empty_dirs:
        (data / rel).mkdir(parents=True, exist_ok=True)
    for path in [data, *data.rglob('*')]:
        if not path.is_symlink():
            os.chmod(path, 0o700 if path.is_dir() else 0o600)
    for name in ('home', 'home/Downloads', 'state', 'config', 'cache', 'share', 'tmp', 'run'):
        (root / name).mkdir(mode=0o700, exist_ok=True)
    (root / 'home' / 'outside-secret.txt').write_bytes(b'must never be served\n')
    (root / 'config' / 'user-dirs.dirs').write_text('XDG_DOWNLOAD_DIR="$HOME/Downloads"\n')
    frontend = root / 'frontend'
    for rel, raw in FRONTEND.items():
        path = frontend / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(raw)
    (frontend / 'escape.txt').symlink_to(root / 'home' / 'outside-secret.txt')
    (frontend / 'inside-link.css').symlink_to(frontend / 'assets' / 'app.css')
    (frontend / 'assets' / 'emptydir').mkdir()
    fonts = root / 'state' / 'samsung-notes-research' / 'ui-reference' / 'fonts'
    fonts.mkdir(parents=True, mode=0o700)
    for parent in (fonts.parent, fonts.parent.parent):
        os.chmod(parent, 0o700)
    def put(name, raw, mode=0o600):
        (fonts / name).write_bytes(raw); os.chmod(fonts / name, mode)
    put('segoeui.ttf', FONT_VALID)
    put('segoeuib.ttf', FONT_VALID + b'b')                 # manifest hash mismatch
    put('seguisb.ttf', FONT_VALID + b's', 0o644)            # group/other readable -> refused
    (fonts / 'segoeuisl.ttf').symlink_to(fonts / 'segoeui.ttf')  # symlink -> refused (O_NOFOLLOW)
    entries = [{'file': 'segoeui.ttf', 'bytes': len(FONT_VALID), 'sha256': hashlib.sha256(FONT_VALID).hexdigest()},
               {'file': 'segoeuib.ttf', 'bytes': len(FONT_VALID) + 1, 'sha256': '0' * 64},
               {'file': 'seguisb.ttf', 'bytes': len(FONT_VALID) + 1, 'sha256': hashlib.sha256(FONT_VALID + b's').hexdigest()},
               {'file': 'segoeuisl.ttf', 'bytes': len(FONT_VALID), 'sha256': hashlib.sha256(FONT_VALID).hexdigest()},
               {'file': 'segoeuil.ttf', 'bytes': 10, 'sha256': '1' * 64}]  # listed but missing
    put('manifest.json', json.dumps({'schemaVersion': 1, 'files': entries}).encode())
    return {'root': root, 'data': data, 'frontend': frontend, 'runtime': root / 'run' / 'runtime.json',
            'home': root / 'home', 'downloads': root / 'home' / 'Downloads'}


def split_command(prefix):
    words = shlex.split(prefix)
    env = {}
    while words and re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*=.*', words[0]):
        key, value = words.pop(0).split('=', 1)
        env[key] = value
    if not words:
        raise SystemExit(f'empty command: {prefix!r}')
    return env, words


def start_server(prefix, layout, log_path):
    extra_env, words = split_command(prefix)
    root = layout['root']
    env = {'PATH': HARNESS['path'] or os.environ.get('PATH', '/usr/bin:/bin'), 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8', 'TZ': 'UTC',
           'HOME': str(root / 'home'), 'XDG_STATE_HOME': str(root / 'state'), 'XDG_CONFIG_HOME': str(root / 'config'),
           'XDG_CACHE_HOME': str(root / 'cache'), 'XDG_DATA_HOME': str(root / 'share'), 'TMPDIR': str(root / 'tmp'),
           'XDG_RUNTIME_DIR': str(root / 'run'), 'PYTHONDONTWRITEBYTECODE': '1', 'PYTHONHASHSEED': '0',
           'SOURCE_DATE_EPOCH': '1700000000',
           # The optional canvas-cache decoder of the historical reference ran a
           # research venv found on one machine only; point it nowhere so every
           # environment (and the goldens) sees providerAvailable: false, which
           # is also what the pure engine reports since 2026-10-27.
           'SAMSUNG_NOTES_PREVIEW_PYTHON': '/nonexistent/preview-python',
           'SAMSUNG_NOTES_PREVIEW_LIBRARY': '/nonexistent/libSPenBase.so'}
    env.update(extra_env)
    argv = words + ['--offline', '--no-auto-sync', '--data-dir', str(layout['data']), '--port', '0',
                    '--runtime-file', str(layout['runtime']), '--no-browser', '--frontend-dir', str(layout['frontend'])]
    log = open(log_path, 'wb')
    process = subprocess.Popen(argv, cwd=ROOT, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                               start_new_session=True)
    PROCESSES.append(process)
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f'server exited with {process.returncode}; log: {log_path}\n' + _tail(log_path))
        try:
            value = json.loads(layout['runtime'].read_text())
            if {'port', 'token', 'pid'} <= set(value):
                return process, value
        except (OSError, ValueError):
            pass
        time.sleep(0.05)
    stop_server(process)
    raise RuntimeError(f'runtime file did not appear: {log_path}\n' + _tail(log_path))


def _tail(path, size=2000):
    try:
        return Path(path).read_bytes()[-size:].decode(errors='replace')
    except OSError:
        return ''


def stop_server(process):
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(10)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(5)
    return process.returncode


@atexit.register
def _kill_all():
    for process in PROCESSES:
        try:
            stop_server(process)
        except Exception:
            pass


# ---------------------------------------------------------------------------
# Raw HTTP
# ---------------------------------------------------------------------------

class Ctx:
    """Per-server request context: port, token, ids by role, captured values."""

    def __init__(self, label, runtime, layout, scenario):
        self.label, self.port, self.token = label, runtime['port'], runtime['token']
        self.layout = layout
        self.notes = dict(scenario['notes'])
        self.folders = dict(scenario.get('folders', {}))
        self.extra = scenario.get('extra', {})
        self.revs = {}
        self.vars = {}

    @property
    def host(self):
        return f'127.0.0.1:{self.port}'

    @property
    def origin(self):
        return f'http://127.0.0.1:{self.port}'

    def nid(self, role):
        return self.notes.get(role) or self.vars[role]

    def rev(self, role):
        return self.revs[self.nid(role)]


def build_request(method, path, *, body=None, ctype='application/json', headers=None, auth=True, host=True,
                  version='HTTP/1.1', length=True, extra=()):
    """Return a builder ctx -> raw request bytes. Header order is exact."""
    def build(c):
        target = path(c) if callable(path) else path
        raw_body = body(c) if callable(body) else body
        if isinstance(raw_body, (dict, list)):
            raw_body = json.dumps(raw_body, ensure_ascii=False).encode()
        elif isinstance(raw_body, str):
            raw_body = raw_body.encode()
        if headers is not None:
            lines = headers(c) if callable(headers) else headers
        else:
            lines = []
            if host:
                lines.append(('Host', c.host))
            if auth:
                lines.append(('Authorization', 'Bearer ' + c.token))
            if raw_body is not None and ctype:
                lines.append(('Content-Type', ctype))
            if raw_body is not None and length:
                lines.append(('Content-Length', str(len(raw_body))))
            lines += [(k, v(c) if callable(v) else v) for k, v in extra]
        head = f'{method} {target} {version}\r\n'.encode('utf-8', 'surrogateescape') if version else f'{method} {target}\r\n'.encode()
        if version:
            for key, value in lines:
                head += key.encode() + b': ' + value.encode('utf-8', 'surrogateescape') + b'\r\n'
            head += b'\r\n'
        return head + (raw_body or b'')
    return build


def exchange(port, raw, *, half_close=False):
    try:
        sock = socket.create_connection(('127.0.0.1', port), timeout=SOCKET_TIMEOUT)
    except OSError as error:
        return f'<<connect failed: {type(error).__name__}>>'.encode()
    with sock:
        try:
            sock.sendall(raw)
            if half_close:
                sock.shutdown(socket.SHUT_WR)
        except (BrokenPipeError, ConnectionResetError):
            pass
        chunks = []
        while True:
            try:
                chunk = sock.recv(65536)
            except ConnectionResetError:
                chunks.append(b'<<connection reset>>')
                break
            except socket.timeout:
                chunks.append(b'<<timeout>>')
                break
            if not chunk:
                break
            chunks.append(chunk)
    return b''.join(chunks)


def parse_response(raw):
    if not raw.startswith(b'HTTP/'):
        return {'status': None, 'headers': [], 'body': raw}
    head, sep, body = raw.partition(b'\r\n\r\n')
    lines = head.split(b'\r\n')
    headers = []
    for line in lines[1:]:
        key, _, value = line.partition(b':')
        headers.append((key.decode('latin-1'), value.strip().decode('latin-1')))
    status = lines[0].decode('latin-1')
    code = int(status.split()[1]) if len(status.split()) > 1 and status.split()[1].isdigit() else None
    return {'status': status, 'code': code, 'headers': headers, 'body': body if sep else b''}


def json_body(response):
    if not any(k.lower() == 'content-type' and v.startswith('application/json') for k, v in response['headers']):
        return None
    try:
        return json.loads(response['body'])
    except ValueError:
        return None


# ---------------------------------------------------------------------------
# Masking
# ---------------------------------------------------------------------------

class Masker:
    def __init__(self, known_ids, windows, roots, ports):
        self.known = set(known_ids)
        self.windows = windows      # {'A': (lo, hi), 'B': (lo, hi)} wall-clock ms of each run
        self.roots = roots          # {'A': str(root), 'B': str(root)}
        self.ports = ports
        self.cand_to_ref = {}

    def learn(self, cand_value, ref_value):
        if isinstance(cand_value, str) and isinstance(ref_value, str) and cand_value != ref_value:
            self.cand_to_ref[cand_value] = ref_value

    def text(self, value, side, numbered=True):
        if side == 'B':
            for cand, ref in self.cand_to_ref.items():
                value = value.replace(cand, ref)
        value = value.replace(self.roots[side], '<ROOT>')
        value = value.replace('127.0.0.1:' + str(self.ports[side]), '127.0.0.1:<PORT>')
        lo, hi = self.windows[side]
        def stamp(match):
            digits = match.group(0); number = int(digits)
            if len(digits) == 13 and lo <= number <= hi: return '<ms>'
            if len(digits) == 16 and lo * 1000 <= number <= hi * 1000: return '<us>'
            if len(digits) == 19 and lo * 10**6 <= number <= hi * 10**6: return '<ns>'
            if len(digits) == 10 and lo // 1000 <= number <= hi // 1000: return '<s>'
            return digits
        value = re.sub(r'(?<![0-9])[0-9]{10,19}(?![0-9])', stamp, value)
        seen = {}
        def ident(match):
            identity = match.group(0)
            if identity in self.known:
                return identity
            if not numbered:
                return '<uuid>'
            return seen.setdefault(identity, f'<uuid{len(seen) + 1}>')
        return UUID_RE.sub(ident, value)

    def unknown_ids(self, value, side):
        if side == 'B':
            for cand, ref in self.cand_to_ref.items():
                value = value.replace(cand, ref)
        out = []
        for identity in UUID_RE.findall(value):
            if identity not in self.known and identity not in out:
                out.append(identity)
        return out


def masked_view(response, masker, side):
    text = response['body'].decode('utf-8', 'surrogateescape')
    headers = [(k, masker.text(v, side)) for k, v in response['headers'] if k.lower() != 'date']
    return response['status'], headers, masker.text(text, side)


# ---------------------------------------------------------------------------
# Steps
# ---------------------------------------------------------------------------

class Step:
    def __init__(self, category, name, build, *, poll=False, capture=None, half_close=False, note=None):
        self.category, self.name, self.build = category, name, build
        self.poll, self.capture, self.half_close, self.note = poll, capture, half_close, note


def J(category, name, method, path, body=None, **options):
    keys = ('poll', 'capture', 'half_close', 'note')
    step_options = {k: options.pop(k) for k in keys if k in options}
    return Step(category, name, build_request(method, path, body=body, **options), **step_options)


def note_path(role, action=None, query=''):
    def path(c):
        value = '/api/notes/' + c.nid(role) + ('/' + action if action else '')
        return value + (('?' + query(c)) if callable(query) else (('?' + query) if query else ''))
    return path


def read_steps(scenario, folder_initialized):
    """Every GET route, security and static/font case. Order is fixed."""
    steps = []
    G = lambda cat, name, path, **kw: steps.append(J(cat, name, 'GET', path, **kw))
    # -- global GETs
    for path in ('/api/status', '/api/templates', '/api/preferences', '/api/shortcuts', '/api/folders', '/api/notes'):
        G('global', 'GET ' + path, path)
    queries = ['q=', 'q=%C3%A7', 'q=%C4%B0%C5%9F', 'q=NOT', 'q=%F0%9F%9A%80', 'q=yok-boyle-bir-sey', 'filter=all',
               'filter=favorites', 'filter=trash', 'filter=conflicts', 'filter=bogus', 'folder=', 'folder=unknown-folder',
               'q=' + 'a' * 1025, 'q=' + 'a' * 1024, 'q=a&q=b', 'q=%ZZ', 'q=%FF', 'filter=favorites&q=%C3%B6',
               '&'.join('x%d=1' % i for i in range(11)), 'q=%00', 'filter=', 'q=+plus+']
    for query in queries:
        G('notes-list', 'GET /api/notes?' + query[:40], '/api/notes?' + query)
    for role, identity in sorted(scenario.get('folders', {}).items()):
        G('notes-list', f'GET /api/notes?folder={role}', '/api/notes?folder=' + identity)
        G('notes-list', f'GET /api/notes?folder={role}&filter=trash', '/api/notes?filter=trash&folder=' + identity)
    # -- per note
    for role in sorted(scenario['notes']):
        def capture_rev(c, response, role=role):
            value = json_body(response)
            if response.get('code') == 200 and value and 'note' in value:
                c.revs[c.nid(role)] = value['note']['revision']
        G('note', f'GET note {role}', note_path(role), capture=capture_rev)
        for action in ('editor', 'ink', 'ink-draft', 'page-style', 'page-ops', 'shapes', 'pages', 'export'):
            G('note-' + action, f'GET {action} {role}', note_path(role, action))
        for query in ('page=0', 'page=1', 'page=2', 'page=999', 'page=abc', '', 'page=0&page=1', 'page=-1', 'page=00001'):
            G('preview', f'GET preview?{query} {role}', note_path(role, 'preview', query), poll=True)
        G('thumbnail', f'GET thumbnail current {role}', note_path(role, 'thumbnail', lambda c, r=role: f'revision={c.rev(r)}'), poll=True)
        G('thumbnail', f'GET thumbnail stale {role}', note_path(role, 'thumbnail', lambda c, r=role: f'revision={c.rev(r) + 1}'))
        G('thumbnail', f'GET thumbnail invalid {role}', note_path(role, 'thumbnail', 'revision=0'))
        pages = scenario.get('extra', {}).get(role, {}).get('pages') or []
        if pages:
            page = pages[0]['id']
            G('ink-geometry', f'GET ink-geometry valid {role}', note_path(role, 'ink-geometry', lambda c, r=role, p=page: f'page={p}&revision={c.rev(r)}'))
            G('ink-geometry', f'GET ink-geometry stale {role}', note_path(role, 'ink-geometry', lambda c, r=role, p=page: f'page={p}&revision={c.rev(r) + 1}'))
            G('ink-geometry', f'GET ink-geometry unknown page {role}', note_path(role, 'ink-geometry', lambda c, r=role: f'page=00000000-0000-4000-8000-00000000dead&revision={c.rev(r)}'))
        G('ink-geometry', f'GET ink-geometry missing {role}', note_path(role, 'ink-geometry', 'page=x'))
    # -- route edge cases
    unknown = '00000000-0000-4000-8000-0000000fffff'
    for path in ('/api/notes/' + unknown, '/api/notes/' + unknown + '/editor', '/api/notes/' + unknown + '/ink',
                 '/api/notes/' + unknown + '/ink-draft', '/api/notes/' + unknown + '/pages', '/api/notes/' + unknown + '/page-ops',
                 '/api/notes/' + unknown + '/page-style', '/api/notes/' + unknown + '/export',
                 '/api/notes/' + unknown + '/preview', '/api/notes/' + unknown + '/thumbnail?revision=1',
                 '/api/notes/' + unknown + '/ink-geometry?page=' + unknown + '&revision=1',
                 '/api/notes/not-a-uuid', '/api/notes/' + unknown.upper(), '/api/notes/' + unknown + '/unknown',
                 '/api/notes/' + unknown + '/', '/api/notes/%30' + unknown[1:], '/api/nope', '/api/', '/api',
                 '/api/status/', '/api/status?x=1', '/api/folders/abc', '/api/notes/editor', '/api//status'):
        G('route', 'GET ' + path[:70], path)
    for method in ('DELETE', 'OPTIONS', 'PUT', 'PATCH', 'POST'):
        steps.append(J('route', f'{method} /api/status', method, '/api/status', body={} if method in ('PUT', 'PATCH', 'POST') else None))
    steps.append(J('route', 'HEAD /api/status', 'HEAD', '/api/status'))
    steps.append(J('route', 'DELETE /api/notes', 'DELETE', '/api/notes'))
    steps.append(J('route', 'GET absolute-form', 'GET', lambda c: f'http://127.0.0.1:{c.port}/api/status'))
    # -- static files
    for path in ('/', '/index.html', '/assets/app.js', '/assets/app.css', '/icons/logo.svg', '/assets/nested/deep/data.json',
                 '/robots.txt', '/file.weird', '/fonts/x.woff2', '/manifest.webmanifest', '/Bo%C5%9Fluk%20dosya.txt',
                 '/assets/map.js.map', '/some/spa/route', '/notes/123', '/missing.js', '/favicon.ico', '/assets',
                 '/assets/emptydir', '/assets/', '/index.html?x=1#frag', '/escape.txt', '/inside-link.css',
                 '/%2e%2e/data/library.sqlite3', '/../data/library.sqlite3', '/assets/../index.html',
                 '/..%2f..%2fhome%2foutside-secret.txt', '/%00', '/index.html%00.js', '/%2Fetc%2Fpasswd', '//etc/passwd',
                 '/assets/app.js/', '/ASSETS/APP.JS', '/%C3%A7', '/%FF', '/a%2Fb', '/.hidden', '/assets/app.js;x'):
        G('static', 'GET ' + path, path, auth=False)
    for path in ('/', '/assets/app.css', '/missing.js', '/local-fonts/segoeui.ttf'):
        steps.append(J('static', 'HEAD ' + path, 'HEAD', path, auth=False))
    for method in ('POST', 'PUT', 'DELETE', 'OPTIONS'):
        steps.append(J('static', f'{method} /', method, '/', body={} if method in ('POST', 'PUT') else None, auth=False))
    G('static', 'GET / with token', '/')
    G('static', 'GET / bad host', '/', auth=False, headers=[('Host', 'evil.invalid')])
    # -- local fonts
    for name in ('segoeui.ttf', 'segoeuib.ttf', 'segoeuil.ttf', 'seguisb.ttf', 'segoeuisl.ttf', 'other.ttf',
                 '..%2Fmanifest.json', 'manifest.json', '', 'segoeui.ttf?x=1', 'SEGOEUI.TTF', 'segoeui.ttf/'):
        G('fonts', 'GET /local-fonts/' + name, '/local-fonts/' + name, auth=False)
    G('fonts', 'GET /local-fonts (no slash)', '/local-fonts', auth=False)
    # -- security
    S = lambda name, method, path, **kw: steps.append(J('security', name, method, path, **kw))
    S('no token', 'GET', '/api/status', auth=False)
    S('wrong token', 'GET', '/api/status', headers=lambda c: [('Host', c.host), ('Authorization', 'Bearer wrong')])
    S('lowercase bearer', 'GET', '/api/status', headers=lambda c: [('Host', c.host), ('Authorization', 'bearer ' + c.token)])
    S('token with space', 'GET', '/api/status', headers=lambda c: [('Host', c.host), ('Authorization', 'Bearer  ' + c.token)])
    S('two auth headers', 'GET', '/api/status', headers=lambda c: [('Host', c.host), ('Authorization', 'Bearer ' + c.token), ('Authorization', 'Bearer ' + c.token)])
    S('auth header casing', 'GET', '/api/status', headers=lambda c: [('host', c.host), ('AUTHORIZATION', 'Bearer ' + c.token)])
    S('bad host', 'GET', '/api/status', headers=lambda c: [('Host', 'example.invalid'), ('Authorization', 'Bearer ' + c.token)])
    S('localhost host', 'GET', '/api/status', headers=lambda c: [('Host', f'localhost:{c.port}'), ('Authorization', 'Bearer ' + c.token)])
    S('host without port', 'GET', '/api/status', headers=lambda c: [('Host', '127.0.0.1'), ('Authorization', 'Bearer ' + c.token)])
    S('duplicate host', 'GET', '/api/status', headers=lambda c: [('Host', c.host), ('Host', c.host), ('Authorization', 'Bearer ' + c.token)])
    S('missing host', 'GET', '/api/status', headers=lambda c: [('Authorization', 'Bearer ' + c.token)])
    S('host trailing space', 'GET', '/api/status', headers=lambda c: [('Host', c.host + ' '), ('Authorization', 'Bearer ' + c.token)])
    S('origin ok', 'GET', '/api/status', extra=[('Origin', lambda c: c.origin)])
    S('origin evil', 'GET', '/api/status', extra=[('Origin', 'https://example.invalid')])
    S('origin null', 'GET', '/api/status', extra=[('Origin', 'null')])
    S('origin empty', 'GET', '/api/status', extra=[('Origin', '')])
    S('origin localhost', 'GET', '/api/status', extra=[('Origin', lambda c: f'http://localhost:{c.port}')])
    S('sec-fetch cross-site', 'GET', '/api/status', extra=[('Sec-Fetch-Site', 'cross-site')])
    S('sec-fetch same-origin', 'GET', '/api/status', extra=[('Sec-Fetch-Site', 'same-origin')])
    S('sec-fetch none', 'GET', '/api/status', extra=[('Sec-Fetch-Site', 'none')])
    S('static cross-site', 'GET', '/', auth=False, extra=[('Sec-Fetch-Site', 'cross-site')])
    S('font evil origin', 'GET', '/local-fonts/segoeui.ttf', auth=False, extra=[('Origin', 'https://example.invalid')])
    # bodies (POST /api/sync validates the body, then answers started:false offline)
    B = lambda name, raw, **kw: steps.append(Step('body', name, build_request('POST', '/api/sync', body=raw, **kw)))
    B('sync empty object', b'{}')
    B('json charset param', b'{}', ctype='application/json; charset=utf-8')
    B('json uppercase type', b'{}', ctype='Application/JSON')
    B('text/plain', b'{}', ctype='text/plain')
    B('no content-type', b'{}', ctype=None)
    B('form type', b'a=1', ctype='application/x-www-form-urlencoded')
    B('no content-length', b'{}', length=False)
    B('zero length', b'')
    B('length +5', b'', length=False, extra=[('Content-Length', '+5')])
    B('length 9 digits', b'', length=False, extra=[('Content-Length', '100000000')])
    B('too large', b'', length=False, extra=[('Content-Length', '3000000')])
    B('max+1', b'', length=False, extra=[('Content-Length', str(2 * 1024 * 1024 + 1))])
    B('two lengths', b'{}', extra=[('Content-Length', '2')])
    B('chunked', b'2\r\n{}\r\n0\r\n\r\n', length=False, extra=[('Transfer-Encoding', 'chunked')])
    B('chunked with length', b'{}', extra=[('Transfer-Encoding', 'chunked')])
    B('invalid json', b'{"a":')
    B('trailing garbage', b'{} x')
    B('duplicate keys', b'{"a": 1, "a": 2}')
    B('nested duplicate keys', b'{"a": {"b": 1, "b": 1}}')
    B('NaN', b'{"a": NaN}')
    B('Infinity', b'{"a": -Infinity}')
    B('array top level', b'[]')
    B('string top level', b'"x"')
    B('null top level', b'null')
    B('invalid utf-8', b'{"a": "\xff"}')
    B('utf-8 bom', b'\xef\xbb\xbf{}')
    B('utf-16 json', '{"a": "ç"}'.encode('utf-16'))
    B('utf-16-le no bom', '{"a": 1}'.encode('utf-16-le'))
    B('lone surrogate escape', b'{"a": "\\ud800"}')
    B('deep nesting', b'[' * 100000 + b']' * 100000)
    B('deep object nesting', b'{"a":' * 3000 + b'1' + b'}' * 3000)
    B('big int', b'{"a": ' + b'9' * 5000 + b'}')
    B('unicode keys', '{"ş": "ğ😀"}'.encode())
    B('whitespace', b' \n\t{ }\n')
    B('expect 100-continue', b'{}', extra=[('Expect', '100-continue')])
    steps.append(Step('body', 'incomplete body', build_request('POST', '/api/sync', body=b'{}', length=False,
                      extra=[('Content-Length', '10')]), half_close=True))
    # -- malformed requests (wire level)
    W = lambda name, raw: steps.append(Step('wire', name, (lambda c, raw=raw: raw(c) if callable(raw) else raw)))
    W('garbage request line', b'GARBAGE\r\n\r\n')
    W('bad version', lambda c: f'GET / HTTP/2.0\r\nHost: {c.host}\r\n\r\n'.encode())
    W('bad version format', lambda c: f'GET / HTTX/1.1\r\nHost: {c.host}\r\n\r\n'.encode())
    W('http/1.0', lambda c: f'GET /api/status HTTP/1.0\r\nHost: {c.host}\r\nAuthorization: Bearer {c.token}\r\n\r\n'.encode())
    W('http/0.9', b'GET /\r\n')
    W('http/0.9 post', b'POST /\r\n')
    W('lowercase method', lambda c: f'get / HTTP/1.1\r\nHost: {c.host}\r\n\r\n'.encode())
    W('unknown method', lambda c: f'BREW / HTTP/1.1\r\nHost: {c.host}\r\n\r\n'.encode())
    W('four words', lambda c: f'GET / HTTP/1.1 extra\r\nHost: {c.host}\r\n\r\n'.encode())
    W('empty line only', b'\r\n')
    W('request line too long', b'GET /' + b'a' * (65537 - 5))
    W('header line too long', lambda c: f'GET / HTTP/1.1\r\nHost: {c.host}\r\nX-Long: '.encode() + b'b' * (65537 - 8))
    W('101 headers', lambda c: f'GET / HTTP/1.1\r\nHost: {c.host}\r\n'.encode() + b''.join(b'X-H%d: 1\r\n' % i for i in range(100)))
    W('100 headers ok', lambda c: f'GET / HTTP/1.1\r\nHost: {c.host}\r\n'.encode() + b''.join(b'X-H%d: 1\r\n' % i for i in range(99)) + b'\r\n')
    W('folded header', lambda c: f'GET /api/status HTTP/1.1\r\nHost: {c.host}\r\nAuthorization: Bearer\r\n {c.token}\r\n\r\n'.encode())
    W('header without colon', lambda c: f'GET / HTTP/1.1\r\nHost: {c.host}\r\nBroken header\r\n\r\n'.encode())
    W('LF only', lambda c: f'GET /api/status HTTP/1.1\nHost: {c.host}\nAuthorization: Bearer {c.token}\n\n'.encode())
    W('non-ascii path', lambda c: f'GET /ç HTTP/1.1\r\nHost: {c.host}\r\n\r\n'.encode())
    W('space in path', lambda c: f'GET /a b HTTP/1.1\r\nHost: {c.host}\r\n\r\n'.encode())
    return steps


def write_steps(scenario):
    """Offline-queued writes in a fixed order, each followed by observing GETs."""
    steps = []
    extra = scenario['extra']
    source = scenario['sourceStrokeId']
    def W(category, name, method, path, body=None, **kw):
        steps.append(J(category, name, method, path, body=body, **kw))
    def observe(role, *actions):
        W('observe', f'GET note {role}', 'GET', note_path(role), capture=capture_note(role))
        for action in actions:
            W('observe', f'GET {action} {role}', 'GET', note_path(role, action))
    def capture_new(var):
        def capture(c, response):
            value = json_body(response)
            if value and 'note' in value:
                c.vars[var] = value['note']['id']; c.revs[value['note']['id']] = value['note']['revision']
        return capture
    def capture_note(role):
        def capture(c, response):
            value = json_body(response)
            if response.get('code') == 200 and value and 'note' in value:
                c.revs[c.nid(role)] = value['note']['revision']
            elif response.get('code') == 200 and value and 'revision' in value:
                c.revs[c.nid(role)] = value['revision']
        return capture
    lists = lambda: [W('observe', 'GET ' + p, 'GET', p) for p in
                     ('/api/notes', '/api/notes?filter=favorites', '/api/notes?filter=trash', '/api/notes?filter=conflicts', '/api/status', '/api/folders')]
    rev = lambda role, delta=0: (lambda c: c.rev(role) + delta)

    # -- preferences / shortcuts
    W('prefs', 'PUT preferences valid', 'PUT', '/api/preferences', {'version': 1, 'galleryMode': 'grid', 'sortAscending': True, 'toolsOpen': False, 'defaultZoom': 150})
    W('prefs', 'PUT preferences invalid zoom', 'PUT', '/api/preferences', {'version': 1, 'galleryMode': 'grid', 'sortAscending': True, 'toolsOpen': False, 'defaultZoom': 110})
    W('prefs', 'PUT preferences bool as int', 'PUT', '/api/preferences', {'version': 1, 'galleryMode': 'grid', 'sortAscending': 1, 'toolsOpen': False, 'defaultZoom': 100})
    W('prefs', 'PUT preferences missing key', 'PUT', '/api/preferences', {'version': 1})
    W('prefs', 'GET preferences', 'GET', '/api/preferences')
    W('prefs', 'PUT shortcuts valid', 'PUT', '/api/shortcuts', {'schemaVersion': 1, 'bindings': {'newNote': 'Ctrl+M', 'searchNotes': 'Ctrl+Shift+F', 'saveNote': 'Ctrl+G'}})
    W('prefs', 'PUT shortcuts reserved', 'PUT', '/api/shortcuts', {'schemaVersion': 1, 'bindings': {'newNote': 'Ctrl+A', 'searchNotes': 'Ctrl+K', 'saveNote': 'Ctrl+S'}})
    W('prefs', 'PUT shortcuts conflict', 'PUT', '/api/shortcuts', {'schemaVersion': 1, 'bindings': {'newNote': 'Ctrl+K', 'searchNotes': 'Ctrl+K', 'saveNote': 'Ctrl+S'}})
    W('prefs', 'PUT shortcuts reserved chord', 'PUT', '/api/shortcuts', {'schemaVersion': 1, 'bindings': {'newNote': 'Ctrl+Shift+N', 'searchNotes': 'Ctrl+K', 'saveNote': 'Ctrl+S'}})
    _SC = DEFAULT_SHORTCUTS
    W('prefs', 'PUT shortcuts v2 valid', 'PUT', '/api/shortcuts', {'schemaVersion': 2, 'bindings': {**_SC, 'bold': 'Ctrl+Shift+B', 'zoomIn': '', 'printNote': 'Alt+Shift+P'}})
    W('prefs', 'PUT shortcuts v2 conflict', 'PUT', '/api/shortcuts', {'schemaVersion': 2, 'bindings': {**_SC, 'italic': 'Ctrl+Shift+B', 'bold': 'Ctrl+Shift+B'}})
    W('prefs', 'PUT shortcuts v2 reserved', 'PUT', '/api/shortcuts', {'schemaVersion': 2, 'bindings': {**_SC, 'bold': 'Ctrl+Z'}})
    W('prefs', 'PUT shortcuts v1 upgrade', 'PUT', '/api/shortcuts', {'schemaVersion': 1, 'bindings': {'newNote': 'Ctrl+N', 'searchNotes': 'Ctrl+K', 'saveNote': 'Ctrl+Shift+M'}})
    W('prefs', 'GET shortcuts', 'GET', '/api/shortcuts')
    # -- create / save plain notes
    W('create', 'POST note', 'POST', '/api/notes', {'title': 'Yeni not 🆕', 'body': 'Gövde\nikinci satır ğüş'}, capture=capture_new('new1'))
    observe('new1', 'editor', 'ink', 'page-style', 'page-ops', 'pages', 'export')
    W('create', 'POST note empty object', 'POST', '/api/notes', {}, capture=capture_new('new2'))
    W('create', 'POST note lone surrogate', 'POST', '/api/notes', b'{"title": "bad", "body": "\\ud800"}')
    W('create', 'POST note control char', 'POST', '/api/notes', {'title': 'a\u0001', 'body': ''})
    W('create', 'POST note title int', 'POST', '/api/notes', {'title': 5, 'body': ''})
    W('create', 'POST note too large', 'POST', '/api/notes', {'title': '', 'body': 'x' * (256 * 1024 + 1)})
    W('create', 'POST note extra keys', 'POST', '/api/notes', {'title': 'Ek', 'body': 'alan', 'other': 1}, capture=capture_new('new3'))
    lists()
    W('save', 'PUT new1', 'PUT', note_path('new1'), lambda c: {'title': 'Yeni not 🆕', 'body': 'Düzenlendi ✓', 'expectedRevision': c.rev('new1')}, capture=capture_note('new1'))
    W('save', 'PUT new1 stale', 'PUT', note_path('new1'), {'title': '', 'body': 'stale', 'expectedRevision': 1})
    W('save', 'PUT new1 same text', 'PUT', note_path('new1'), lambda c: {'title': 'Yeni not 🆕', 'body': 'Düzenlendi ✓', 'expectedRevision': c.rev('new1')})
    W('save', 'PUT new1 no revision', 'PUT', note_path('new1'), {'title': 'x', 'body': 'y'})
    W('save', 'PUT new1 revision string', 'PUT', note_path('new1'), lambda c: {'title': 'x', 'body': 'y', 'expectedRevision': str(c.rev('new1'))})
    W('save', 'PUT cloud_edit', 'PUT', note_path('cloud_edit'), lambda c: {'title': 'Düzenlenecek', 'body': 'Yerel değişiklik 🙂', 'expectedRevision': c.rev('cloud_edit')}, capture=capture_note('cloud_edit'))
    observe('cloud_edit', 'editor')
    W('save', 'PUT cloud_edit back to base', 'PUT', note_path('cloud_edit'), lambda c: {'title': 'Düzenlenecek', 'body': 'İlk bulut metni', 'expectedRevision': c.rev('cloud_edit')}, capture=capture_note('cloud_edit'))
    W('save', 'PUT readonly', 'PUT', note_path('cloud_readonly'), lambda c: {'title': 'Kitap', 'body': 'x', 'expectedRevision': c.rev('cloud_readonly')})
    W('save', 'PUT conflict note', 'PUT', note_path('conflict'), lambda c: {'title': 'Çakışan not', 'body': 'x', 'expectedRevision': c.rev('conflict')})
    W('save', 'PUT ink_pending', 'PUT', note_path('ink_pending'), lambda c: {'title': 'x', 'body': 'y', 'expectedRevision': c.rev('ink_pending')})
    W('save', 'PUT stub', 'PUT', note_path('stub'), lambda c: {'title': 'x', 'body': 'y', 'expectedRevision': c.rev('stub')})
    W('save', 'PUT local_edit', 'PUT', note_path('local_edit'), lambda c: {'title': 'Yerel düzenlendi', 'body': 'Sonra bu', 'expectedRevision': c.rev('local_edit')}, capture=capture_note('local_edit'))
    W('save', 'PUT trashed', 'PUT', note_path('cloud_trash'), lambda c: {'title': 'x', 'body': 'y', 'expectedRevision': c.rev('cloud_trash')})
    # -- title
    W('title', 'PATCH title cloud_title', 'PATCH', note_path('cloud_title', 'title'), lambda c: {'title': 'Yeni başlık ✨', 'expectedRevision': c.rev('cloud_title')}, capture=capture_note('cloud_title'))
    observe('cloud_title', 'editor')
    W('title', 'PATCH title again pending', 'PATCH', note_path('cloud_title', 'title'), lambda c: {'title': 'Üçüncü', 'expectedRevision': c.rev('cloud_title')})
    W('title', 'PATCH title newline', 'PATCH', note_path('cloud_readonly', 'title'), lambda c: {'title': 'a\nb', 'expectedRevision': c.rev('cloud_readonly')})
    W('title', 'PATCH title with body', 'PATCH', note_path('cloud_readonly', 'title'), lambda c: {'title': 'a', 'body': 'b', 'expectedRevision': c.rev('cloud_readonly')})
    W('title', 'PATCH title readonly ink', 'PATCH', note_path('cloud_readonly', 'title'), lambda c: {'title': 'Kitap 2', 'expectedRevision': c.rev('cloud_readonly')}, capture=capture_note('cloud_readonly'))
    W('title', 'PATCH title local', 'PATCH', note_path('local_plain', 'title'), lambda c: {'title': 'x', 'expectedRevision': c.rev('local_plain')})
    W('title', 'PATCH title stale', 'PATCH', note_path('cloud_deep', 'title'), {'title': 'x', 'expectedRevision': 999})
    W('title', 'PATCH title no revision', 'PATCH', note_path('cloud_deep', 'title'), {'title': 'x'})
    W('title', 'PATCH title empty body', 'PATCH', note_path('cloud_deep', 'title'), {})
    # -- editor
    model = {'schemaVersion': 1, 'text': 'A😀B ç', 'spans': [{'start': 1, 'end': 3, 'kind': 'foregroundColor', 'value': '#FF123456'}],
             'paragraphs': [{'index': 0, 'alignment': 'center'}]}
    W('editor', 'POST editor note', 'POST', '/api/notes/editor', {'title': 'Zengin yeni', 'model': model}, capture=capture_new('new_editor'))
    observe('new_editor', 'editor')
    W('editor', 'POST editor missing model', 'POST', '/api/notes/editor', {'title': 'x'})
    W('editor', 'POST editor bad span', 'POST', '/api/notes/editor', {'title': 'x', 'model': {**model, 'spans': [{'start': 2, 'end': 3, 'kind': 'bold', 'value': True}]}})
    W('editor', 'POST editor bad schema', 'POST', '/api/notes/editor', {'title': 'x', 'model': {'schemaVersion': 2, 'text': ''}})
    W('editor', 'PUT editor cloud_rich_edit', 'PUT', note_path('cloud_rich_edit', 'editor'), lambda c: {'title': 'Zengin düzenlendi', 'model': model, 'expectedRevision': c.rev('cloud_rich_edit')}, capture=capture_note('cloud_rich_edit'))
    observe('cloud_rich_edit', 'editor')
    W('editor', 'PUT editor stale', 'PUT', note_path('cloud_rich', 'editor'), {'title': 'x', 'model': model, 'expectedRevision': 999})
    W('editor', 'PUT editor missing key', 'PUT', note_path('cloud_rich', 'editor'), lambda c: {'title': 'x', 'model': model})
    W('editor', 'PUT editor readonly', 'PUT', note_path('cloud_readonly', 'editor'), lambda c: {'title': 'x', 'model': model, 'expectedRevision': c.rev('cloud_readonly')})
    W('editor', 'PUT editor local_editor', 'PUT', note_path('local_editor', 'editor'), lambda c: {'title': 'Yerel zengin 2', 'model': model, 'expectedRevision': c.rev('local_editor')}, capture=capture_note('local_editor'))
    # -- organize (folder_initialized)
    folders = scenario['folders']
    W('organize', 'PATCH favorite', 'PATCH', note_path('cloud_favtoggle'), lambda c: {'favorite': True, 'expectedRevision': c.rev('cloud_favtoggle')}, capture=capture_note('cloud_favtoggle'))
    W('organize', 'PATCH favorite pending again', 'PATCH', note_path('cloud_favtoggle'), lambda c: {'favorite': False, 'expectedRevision': c.rev('cloud_favtoggle')})
    W('organize', 'PATCH trash', 'PATCH', note_path('cloud_trashtoggle'), lambda c: {'trashed': True, 'expectedRevision': c.rev('cloud_trashtoggle')}, capture=capture_note('cloud_trashtoggle'))
    W('organize', 'PATCH move', 'PATCH', note_path('cloud_move'), lambda c: {'folderId': folders['empty'], 'expectedRevision': c.rev('cloud_move')}, capture=capture_note('cloud_move'))
    W('organize', 'PATCH move pending folder', 'PATCH', note_path('cloud_deep'), lambda c: {'folderId': folders['pending']})
    W('organize', 'PATCH move unknown folder', 'PATCH', note_path('cloud_deep'), {'folderId': 'no-such-folder'})
    W('organize', 'PATCH move root', 'PATCH', note_path('cloud_deep'), {'folderId': None}, capture=capture_note('cloud_deep'))
    W('organize', 'PATCH same value', 'PATCH', note_path('cloud_plain'), {'favorite': False})
    W('organize', 'PATCH two fields', 'PATCH', note_path('cloud_plain'), {'favorite': True, 'trashed': True})
    W('organize', 'PATCH favorite non-bool', 'PATCH', note_path('cloud_plain'), {'favorite': 1})
    W('organize', 'PATCH stale', 'PATCH', note_path('cloud_plain'), {'favorite': True, 'expectedRevision': 999})
    W('organize', 'PATCH local note', 'PATCH', note_path('local_plain'), {'favorite': True})
    W('organize', 'PATCH stub', 'PATCH', note_path('stub'), {'favorite': True})
    W('organize', 'PATCH deleted remote', 'PATCH', note_path('deleted_remote'), {'favorite': True})
    W('organize', 'PATCH conflict note', 'PATCH', note_path('conflict'), {'favorite': True})
    W('organize', 'PATCH unknown note', 'PATCH', '/api/notes/00000000-0000-4000-8000-0000000fffff', {'favorite': True})
    lists()
    # -- folders
    W('folders', 'POST folder', 'POST', '/api/folders', {'name': ' Yeni klasör 📁 '}, capture=lambda c, r: c.vars.update(new_folder=(json_body(r) or {}).get('folder', {}).get('id')))
    W('folders', 'POST folder nested', 'POST', '/api/folders', {'name': 'Alt klasör', 'parentId': folders['work']})
    W('folders', 'POST folder pending parent', 'POST', '/api/folders', {'name': 'x', 'parentId': folders['pending']})
    W('folders', 'POST folder unknown parent', 'POST', '/api/folders', {'name': 'x', 'parentId': 'nope'})
    W('folders', 'POST folder empty name', 'POST', '/api/folders', {'name': '   '})
    W('folders', 'POST folder extra key', 'POST', '/api/folders', {'name': 'x', 'color': 'red'})
    W('folders', 'POST folder name int', 'POST', '/api/folders', {'name': 5})
    W('folders', 'PATCH folder rename', 'PATCH', '/api/folders/' + folders['empty'], {'name': 'Yeniden adlandırıldı'})
    W('folders', 'PATCH folder rename pending', 'PATCH', '/api/folders/' + folders['empty'], {'name': 'İkinci ad'})
    W('folders', 'PATCH folder trash non-empty', 'PATCH', '/api/folders/' + folders['deep'], {'trashed': True})
    W('folders', 'PATCH folder trash empty system', 'PATCH', '/api/folders/' + folders['system'], {'trashed': True})
    W('folders', 'PATCH folder cycle', 'PATCH', '/api/folders/' + folders['work'], {'parentId': folders['projects']})
    W('folders', 'PATCH folder move to root', 'PATCH', '/api/folders/' + folders['deep'], {'parentId': None})
    W('folders', 'PATCH folder restore trashed', 'PATCH', '/api/folders/' + folders['old'], {'trashed': False})
    W('folders', 'PATCH folder unknown', 'PATCH', '/api/folders/unknown-folder', {'name': 'x'})
    W('folders', 'PATCH folder two keys', 'PATCH', '/api/folders/' + folders['work'], {'name': 'x', 'trashed': True})
    W('folders', 'PATCH folder bad utf8 id', 'PATCH', '/api/folders/%FF', {'name': 'x'})
    W('folders', 'PATCH folder control id', 'PATCH', '/api/folders/%01', {'name': 'x'})
    W('folders', 'PATCH folder encoded id', 'PATCH', '/api/folders/' + folders['work'].replace('-', '%2D'), {'name': 'İş Notları'})
    W('folders', 'PATCH new folder pending', 'PATCH', lambda c: '/api/folders/' + str(c.vars.get('new_folder')), {'name': 'x'})
    W('folders', 'POST folder resolve offline', 'POST', '/api/folders/' + folders['pending'] + '/resolve', {'strategy': 'use_remote'})
    W('folders', 'POST folder resolve bad strategy', 'POST', '/api/folders/' + folders['pending'] + '/resolve', {'strategy': 'keep'})
    W('folders', 'POST folder resolve no strategy', 'POST', '/api/folders/' + folders['pending'] + '/resolve', {})
    W('folders', 'GET folders', 'GET', '/api/folders')
    # -- ink draft
    page = lambda role: extra[role]['pages'][0]['id']
    def draft_stroke(role, x=100.0, sid='draft-a'):
        return {'id': sid, 'pageId': page(role), 'sourceStrokeId': source, 'kind': 'pen', 'colorARGB': 0xff123456,
                'size': 2.0, 'pageSize': [1080, 1527], 'points': [{'x': x, 'y': 100.0, 'pressure': 0.5, 'timeMs': 0},
                                                                   {'x': x + 10, 'y': 110.0, 'pressure': 0.6, 'timeMs': 10}]}
    W('ink-draft', 'PUT ink-draft', 'PUT', note_path('ink_view', 'ink-draft'), lambda c: {'revision': c.rev('ink_view'), 'strokes': [draft_stroke('ink_view'), draft_stroke('ink_view', 150.0, 'draft-b')]})
    W('ink-draft', 'GET ink-draft', 'GET', note_path('ink_view', 'ink-draft'))
    W('ink-draft', 'PUT ink-draft bad stroke', 'PUT', note_path('ink_view', 'ink-draft'), lambda c: {'revision': c.rev('ink_view'), 'strokes': [{'id': 'x'}]})
    W('ink-draft', 'PUT ink-draft revision str', 'PUT', note_path('ink_view', 'ink-draft'), {'revision': '1', 'strokes': []})
    W('ink-draft', 'PUT ink-draft extra key', 'PUT', note_path('ink_view', 'ink-draft'), {'revision': 1, 'strokes': [], 'x': 1})
    W('ink-draft', 'PUT ink-draft clear existing', 'PUT', note_path('ink_draft', 'ink-draft'), lambda c: {'revision': c.rev('ink_draft'), 'strokes': []})
    W('ink-draft', 'GET ink-draft cleared', 'GET', note_path('ink_draft', 'ink-draft'))
    W('ink-draft', 'PUT ink-draft unknown note', 'PUT', '/api/notes/00000000-0000-4000-8000-0000000fffff/ink-draft', {'revision': 1, 'strokes': []})
    # -- ink append
    def ink_stroke(role, x=100.0):
        return {'pageId': page(role), 'sourceStrokeId': source, 'points': [
            {'x': x, 'y': 100.0, 'pressure': 0.5, 'timeMs': 0}, {'x': x + 10, 'y': 110.0, 'pressure': 0.6, 'timeMs': 10},
            {'x': x + 25.5, 'y': 130.25, 'pressure': 0.7, 'timeMs': 22}]}
    W('ink', 'PUT ink wrong page', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put'), 'strokes': [{**ink_stroke('ink_put'), 'pageId': 'different-page'}]})
    W('ink', 'PUT ink bad time', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put'), 'strokes': [{**ink_stroke('ink_put'), 'points': [{'x': 1.0, 'y': 1.0, 'pressure': 0.5, 'timeMs': 1}, {'x': 2.0, 'y': 2.0, 'pressure': 0.5, 'timeMs': 2}]}]})
    W('ink', 'PUT ink pressure bool', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put'), 'strokes': [{**ink_stroke('ink_put'), 'points': [{'x': 1.0, 'y': 1.0, 'pressure': True, 'timeMs': 0}, {'x': 2.0, 'y': 2.0, 'pressure': 0.5, 'timeMs': 2}]}]})
    W('ink', 'PUT ink stale', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put') - 1, 'strokes': [ink_stroke('ink_put')]})
    W('ink', 'PUT ink missing key', 'PUT', note_path('ink_put', 'ink'), lambda c: {'strokes': [ink_stroke('ink_put')]})
    def hl_stroke(role, **over):
        return {'tool': 'highlighter', 'pageId': page(role), 'colorARGB': 0x73ffcb30, 'size': 40.0, 'points': [
            {'x': 100.0, 'y': 100.0, 'pressure': 0.5, 'timeMs': 0}, {'x': 140.0, 'y': 102.0, 'pressure': 0.5, 'timeMs': 12}], **over}
    W('ink', 'PUT highlighter opaque colour', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put'), 'strokes': [hl_stroke('ink_put', colorARGB=0xffffcb30)]})
    W('ink', 'PUT highlighter unknown page', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put'), 'strokes': [hl_stroke('ink_put', pageId='different-page')]})
    W('ink', 'PUT highlighter extra key', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put'), 'strokes': [hl_stroke('ink_put', kind='pen')]})
    W('ink', 'PUT highlighter size too small', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put'), 'strokes': [hl_stroke('ink_put', size=0.5)]})
    W('ink', 'PUT highlighter bool size', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put'), 'strokes': [hl_stroke('ink_put', size=True)]})
    W('ink', 'PUT ink valid 2 strokes', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put'), 'strokes': [ink_stroke('ink_put'), ink_stroke('ink_put', 200.0)]}, capture=capture_note('ink_put'))
    observe('ink_put', 'ink', 'ink-draft', 'page-style', 'page-ops', 'pages')
    W('ink', 'PUT ink again pending', 'PUT', note_path('ink_put', 'ink'), lambda c: {'expectedRevision': c.rev('ink_put'), 'strokes': [ink_stroke('ink_put')]})
    W('ink', 'PUT ink read-only note', 'PUT', note_path('cloud_plain', 'ink'), lambda c: {'expectedRevision': c.rev('cloud_plain'), 'strokes': [ink_stroke('ink_put')]})
    W('ink', 'PUT first stroke blank page', 'PUT', note_path('blank_page', 'ink'), lambda c: {'expectedRevision': c.rev('blank_page'), 'strokes': [
        {'pageId': page('blank_page'), 'points': [{'x': 100 + i * 10, 'y': 200, 'pressure': 0.5, 'timeMs': i * 16} for i in range(5)], 'colorARGB': 0xFFFF0000, 'size': 3.0}]}, capture=capture_note('blank_page'))
    observe('blank_page', 'ink')
    W('ink', 'PUT ink draft note deletes draft', 'PUT', note_path('ink_draft', 'ink'), lambda c: {'expectedRevision': c.rev('ink_draft'), 'strokes': [ink_stroke('ink_draft')]}, capture=capture_note('ink_draft'))
    observe('ink_draft', 'ink', 'ink-draft')
    # -- ink erase
    erase_page = extra['ink_erase']['pages'][0]
    erase = lambda ids: {'pageId': erase_page['id'], 'layerId': erase_page['layerId'], 'strokeIds': ids}
    W('ink-erase', 'PUT ink-erase invalid groups', 'PUT', note_path('ink_erase', 'ink-erase'), lambda c: {'erase': [], 'expectedRevision': c.rev('ink_erase')})
    W('ink-erase', 'PUT ink-erase unknown stroke', 'PUT', note_path('ink_erase', 'ink-erase'), lambda c: {'erase': [erase(['00000000-0000-4000-8000-00000000dead'])], 'expectedRevision': c.rev('ink_erase')})
    W('ink-erase', 'PUT ink-erase stale', 'PUT', note_path('ink_erase', 'ink-erase'), lambda c: {'erase': [erase(scenario['eraseStrokes'][:1])], 'expectedRevision': c.rev('ink_erase') + 5})
    W('ink-erase', 'PUT ink-erase extra key', 'PUT', note_path('ink_erase', 'ink-erase'), lambda c: {'erase': [erase(scenario['eraseStrokes'][:1])], 'expectedRevision': c.rev('ink_erase'), 'x': 1})
    W('ink-erase', 'PUT ink-erase valid', 'PUT', note_path('ink_erase', 'ink-erase'), lambda c: {'erase': [erase(scenario['eraseStrokes'][:1])], 'expectedRevision': c.rev('ink_erase')}, capture=capture_note('ink_erase'))
    observe('ink_erase', 'ink')
    W('ink-erase', 'GET ink-geometry after erase', 'GET', note_path('ink_erase', 'ink-geometry', lambda c: f'page={erase_page["id"]}&revision={c.rev("ink_erase")}'))
    W('ink-erase', 'PUT ink-erase again pending', 'PUT', note_path('ink_erase', 'ink-erase'), lambda c: {'erase': [erase(scenario['eraseStrokes'][1:])], 'expectedRevision': c.rev('ink_erase')})
    # -- ink move (lasso)
    move_page = extra['ink_move']['pages'][0]
    move = lambda ids, dx=8, dy=4: {'pageId': move_page['id'], 'layerId': move_page['layerId'], 'strokeIds': ids, 'dx': dx, 'dy': dy}
    W('ink-move', 'PUT ink-move invalid groups', 'PUT', note_path('ink_move', 'ink-move'), lambda c: {'move': [], 'expectedRevision': c.rev('ink_move')})
    W('ink-move', 'PUT ink-move bool offset', 'PUT', note_path('ink_move', 'ink-move'), lambda c: {'move': [move(scenario['eraseStrokes'][:1], True, 4)], 'expectedRevision': c.rev('ink_move')})
    W('ink-move', 'PUT ink-move zero offset', 'PUT', note_path('ink_move', 'ink-move'), lambda c: {'move': [move(scenario['eraseStrokes'][:1], 0, 0)], 'expectedRevision': c.rev('ink_move')})
    W('ink-move', 'PUT ink-move off page', 'PUT', note_path('ink_move', 'ink-move'), lambda c: {'move': [move(scenario['eraseStrokes'][:1], 5000, 0)], 'expectedRevision': c.rev('ink_move')})
    W('ink-move', 'PUT ink-move unknown stroke', 'PUT', note_path('ink_move', 'ink-move'), lambda c: {'move': [move(['00000000-0000-4000-8000-00000000dead'])], 'expectedRevision': c.rev('ink_move')})
    W('ink-move', 'PUT ink-move stale', 'PUT', note_path('ink_move', 'ink-move'), lambda c: {'move': [move(scenario['eraseStrokes'][:1])], 'expectedRevision': c.rev('ink_move') + 5})
    W('ink-move', 'PUT ink-move extra key', 'PUT', note_path('ink_move', 'ink-move'), lambda c: {'move': [move(scenario['eraseStrokes'][:1])], 'expectedRevision': c.rev('ink_move'), 'x': 1})
    W('ink-move', 'PUT ink-move valid', 'PUT', note_path('ink_move', 'ink-move'), lambda c: {'move': [move(scenario['eraseStrokes'], 8, -4)], 'expectedRevision': c.rev('ink_move')}, capture=capture_note('ink_move'))
    observe('ink_move', 'ink')
    W('ink-move', 'PUT ink-move again pending', 'PUT', note_path('ink_move', 'ink-move'), lambda c: {'move': [move(scenario['eraseStrokes'][:1])], 'expectedRevision': c.rev('ink_move')})
    W('ink-move', 'PUT ink-erase while move pending', 'PUT', note_path('ink_move', 'ink-erase'), lambda c: {'erase': [{'pageId': move_page['id'], 'layerId': move_page['layerId'], 'strokeIds': scenario['eraseStrokes'][:1]}], 'expectedRevision': c.rev('ink_move')})
    # -- page style
    W('page-style', 'PUT page-style non-opaque', 'PUT', note_path('page_style', 'page-style'), lambda c: {'backgroundArgb': 0x80F5DDDD, 'expectedRevision': c.rev('page_style')})
    W('page-style', 'PUT page-style negative', 'PUT', note_path('page_style', 'page-style'), lambda c: {'backgroundArgb': -1, 'expectedRevision': c.rev('page_style')})
    W('page-style', 'PUT page-style float', 'PUT', note_path('page_style', 'page-style'), lambda c: {'backgroundArgb': 4294309341.0, 'expectedRevision': c.rev('page_style')})
    W('page-style', 'PUT page-style stale', 'PUT', note_path('page_style', 'page-style'), lambda c: {'backgroundArgb': 0xFFF5DDDD, 'expectedRevision': c.rev('page_style') - 1})
    W('page-style', 'PUT page-style valid', 'PUT', note_path('page_style', 'page-style'), lambda c: {'backgroundArgb': 0xFFF5DDDD, 'expectedRevision': c.rev('page_style')}, capture=capture_note('page_style'))
    observe('page_style', 'page-style', 'ink')
    W('page-style', 'PUT page-style pending', 'PUT', note_path('page_style', 'page-style'), lambda c: {'backgroundArgb': 0xFFDDE7F5, 'expectedRevision': c.rev('page_style')})
    W('page-style', 'PUT page-style plain note', 'PUT', note_path('cloud_plain', 'page-style'), lambda c: {'backgroundArgb': 0xFFDDE7F5, 'expectedRevision': c.rev('cloud_plain')})
    W('page-style', 'PUT page-style local note', 'PUT', note_path('local_plain', 'page-style'), lambda c: {'backgroundArgb': 0xFFDDE7F5, 'expectedRevision': c.rev('local_plain')})
    # -- page ops
    po = lambda role, i: extra[role]['pages'][i]['id']
    W('page-ops', 'POST page-ops bad op', 'POST', note_path('page_ops', 'page-ops'), lambda c: {'op': 'rotate', 'expectedRevision': c.rev('page_ops')})
    W('page-ops', 'POST page-ops add extra key', 'POST', note_path('page_ops', 'page-ops'), lambda c: {'op': 'add', 'afterIndex': 0, 'pageIds': [], 'expectedRevision': c.rev('page_ops')})
    W('page-ops', 'POST page-ops add bad index', 'POST', note_path('page_ops', 'page-ops'), lambda c: {'op': 'add', 'afterIndex': 99, 'expectedRevision': c.rev('page_ops')})
    W('page-ops', 'POST page-ops add', 'POST', note_path('page_ops', 'page-ops'), lambda c: {'op': 'add', 'afterIndex': 0, 'expectedRevision': c.rev('page_ops')}, capture=capture_note('page_ops'))
    observe('page_ops', 'page-ops', 'pages')
    W('page-ops', 'POST page-ops add pending', 'POST', note_path('page_ops', 'page-ops'), lambda c: {'op': 'add', 'afterIndex': 0, 'expectedRevision': c.rev('page_ops')})
    W('page-ops', 'POST page-ops delete', 'POST', note_path('page_ops_delete', 'page-ops'), lambda c: {'op': 'delete', 'pageIds': [po('page_ops_delete', 2)], 'expectedRevision': c.rev('page_ops_delete')}, capture=capture_note('page_ops_delete'))
    observe('page_ops_delete', 'page-ops')
    W('page-ops', 'POST page-ops reorder bad', 'POST', note_path('page_ops_reorder', 'page-ops'), lambda c: {'op': 'reorder', 'pageIds': [po('page_ops_reorder', 0)], 'expectedRevision': c.rev('page_ops_reorder')})
    W('page-ops', 'POST page-ops reorder', 'POST', note_path('page_ops_reorder', 'page-ops'), lambda c: {'op': 'reorder', 'pageIds': [po('page_ops_reorder', i) for i in (2, 0, 1)], 'expectedRevision': c.rev('page_ops_reorder')}, capture=capture_note('page_ops_reorder'))
    observe('page_ops_reorder', 'page-ops')
    W('page-ops', 'POST page-ops plain note', 'POST', note_path('cloud_plain', 'page-ops'), lambda c: {'op': 'add', 'afterIndex': 0, 'expectedRevision': c.rev('cloud_plain')})
    # -- shapes (page-ops op 'shape')
    rect = {'kind': 'rectangle', 'geometry': [100, 120.3, 300, 260], 'colorARGB': 0xffff0000, 'width': 4}
    sp = lambda i: po('shapes_target', i)
    shape_req = lambda shape, role='shapes_target', page=1, **more: (lambda c: {'op': 'shape', 'pageId': po(role, page) if role in extra else '00000000-0000-4000-8000-00000000dead', 'shape': shape, 'expectedRevision': c.rev(role), **more})
    W('shapes', 'POST shape no revision', 'POST', note_path('shapes_target', 'page-ops'), lambda c: {'op': 'shape', 'pageId': sp(1), 'shape': rect})
    W('shapes', 'POST shape extra key', 'POST', note_path('shapes_target', 'page-ops'), shape_req(rect, x=1))
    W('shapes', 'POST shape bad kind', 'POST', note_path('shapes_target', 'page-ops'), shape_req({**rect, 'kind': 'star'}))
    W('shapes', 'POST shape off page', 'POST', note_path('shapes_target', 'page-ops'), shape_req({**rect, 'geometry': [0, 0, 5000, 10]}))
    W('shapes', 'POST shape tiny', 'POST', note_path('shapes_target', 'page-ops'), shape_req({**rect, 'geometry': [5, 5, 5.2, 50]}))
    W('shapes', 'POST shape line fill', 'POST', note_path('shapes_target', 'page-ops'), shape_req({'kind': 'line', 'geometry': [1, 1, 50, 50], 'colorARGB': 0xff000000, 'width': 2, 'fillARGB': 0xff00ff00}))
    W('shapes', 'POST shape bad width', 'POST', note_path('shapes_target', 'page-ops'), shape_req({**rect, 'width': 99}))
    W('shapes', 'POST shape bool color', 'POST', note_path('shapes_target', 'page-ops'), shape_req({**rect, 'colorARGB': True}))
    W('shapes', 'POST shape unknown key', 'POST', note_path('shapes_target', 'page-ops'), shape_req({**rect, 'rotation': 5}))
    W('shapes', 'POST shape not dict', 'POST', note_path('shapes_target', 'page-ops'), shape_req([1, 2]))
    W('shapes', 'POST shape unknown page', 'POST', note_path('shapes_target', 'page-ops'), lambda c: {'op': 'shape', 'pageId': '00000000-0000-4000-8000-00000000dead', 'shape': rect, 'expectedRevision': c.rev('shapes_target')})
    W('shapes', 'POST shape bad page id', 'POST', note_path('shapes_target', 'page-ops'), lambda c: {'op': 'shape', 'pageId': 7, 'shape': rect, 'expectedRevision': c.rev('shapes_target')})
    W('shapes', 'POST shape stale', 'POST', note_path('shapes_target', 'page-ops'), lambda c: {'op': 'shape', 'pageId': sp(1), 'shape': rect, 'expectedRevision': c.rev('shapes_target') + 3})
    W('shapes', 'POST shape plain note', 'POST', note_path('cloud_plain', 'page-ops'), lambda c: {'op': 'shape', 'pageId': '00000000-0000-4000-8000-00000000dead', 'shape': rect, 'expectedRevision': c.rev('cloud_plain')})
    W('shapes', 'POST shape ink note', 'POST', note_path('shapes_drawn', 'page-ops'), lambda c: {'op': 'shape', 'pageId': po('shapes_drawn', 1), 'shape': {'kind': 'ellipse', 'geometry': [10, 10, 90, 60], 'colorARGB': 0xff0000ff, 'width': 2.5, 'fillARGB': 0xffffff00}, 'expectedRevision': c.rev('shapes_drawn')}, capture=capture_note('shapes_drawn'))
    W('shapes', 'POST shape valid', 'POST', note_path('shapes_target', 'page-ops'), lambda c: {'op': 'shape', 'pageId': sp(1), 'shape': rect, 'expectedRevision': c.rev('shapes_target')}, capture=capture_note('shapes_target'))
    observe('shapes_target', 'shapes', 'page-ops', 'ink')
    observe('shapes_drawn', 'shapes')
    W('shapes', 'POST shape pending', 'POST', note_path('shapes_target', 'page-ops'), lambda c: {'op': 'shape', 'pageId': sp(2), 'shape': {**rect, 'kind': 'arrow'}, 'expectedRevision': c.rev('shapes_target')})
    # -- pdf
    pdf = tiny_pdf([(595, 842), (842, 595)])
    def pdf_req(name, query, raw, ctype='application/pdf', **kw):
        steps.append(Step('pdf', name, build_request('POST', note_path('pdf_target', 'pdf', query), body=raw, ctype=ctype), **kw))
    pdf_req('POST pdf no revision', '', pdf)
    pdf_req('POST pdf bad revision', 'revision=0', pdf)
    pdf_req('POST pdf extra param', lambda c: f'revision={c.rev("pdf_target")}&x=1', pdf)
    pdf_req('POST pdf two names', lambda c: f'revision={c.rev("pdf_target")}&name=a.pdf&name=b.pdf', pdf)
    pdf_req('POST pdf wrong type', lambda c: f'revision={c.rev("pdf_target")}', pdf, ctype='application/octet-stream')
    pdf_req('POST pdf not pdf', lambda c: f'revision={c.rev("pdf_target")}', b'not a pdf at all')
    pdf_req('POST pdf broken pdf', lambda c: f'revision={c.rev("pdf_target")}', b'%PDF-1.7 synthetic but broken')
    pdf_req('POST pdf stale', lambda c: f'revision={c.rev("pdf_target") + 1}', pdf)
    pdf_req('POST pdf valid', lambda c: f'revision={c.rev("pdf_target")}&name=..%2F..%2FDers%20Notu', pdf, capture=capture_note('pdf_target'))
    observe('pdf_target', 'pages', 'page-ops', 'ink')
    pdf_req('POST pdf again pending', lambda c: f'revision={c.rev("pdf_target")}', pdf)
    steps.append(Step('pdf', 'POST pdf no length', build_request('POST', note_path('pdf_target', 'pdf', 'revision=1'), body=pdf, ctype='application/pdf', length=False)))
    # -- templates
    template_sha = extra['pdf_template']['pdfSha256']
    W('template', 'POST template unknown', 'POST', note_path('template_target', 'template'), lambda c: {'templateId': '0' * 64, 'expectedRevision': c.rev('template_target')})
    W('template', 'POST template uncached', 'POST', note_path('template_target', 'template'), lambda c: {'templateId': extra['pdf_uncached']['pdfSha256'], 'expectedRevision': c.rev('template_target')})
    W('template', 'POST template missing key', 'POST', note_path('template_target', 'template'), {'templateId': template_sha})
    W('template', 'POST template stale', 'POST', note_path('template_target', 'template'), lambda c: {'templateId': template_sha, 'expectedRevision': c.rev('template_target') + 1})
    W('template', 'POST template valid', 'POST', note_path('template_target', 'template'), lambda c: {'templateId': template_sha, 'expectedRevision': c.rev('template_target')}, capture=capture_note('template_target'))
    observe('template_target', 'pages')
    W('template', 'GET templates after', 'GET', '/api/templates')
    # templates?note= (inNote marking, per-revision cache) and its validation
    for role in ('template_target', 'pdf_template', 'pdf_uncached', 'cloud_plain'):
        W('template', f'GET templates note={role}', 'GET', lambda c, r=role: '/api/templates?note=' + c.nid(r))
    for query in ('note=', 'note=not-a-uuid', 'note=00000000-0000-4000-8000-0000000fffff', 'other=1',
                  'note=00000000-0000-4000-8000-0000000fffff&note=00000000-0000-4000-8000-0000000ffffe'):
        W('template', 'GET templates ' + query[:40], 'GET', '/api/templates?' + query)
    # -- export to file (HOME/Downloads of each server's scratch)
    W('export-file', 'POST export-file txt', 'POST', note_path('cloud_export', 'export-file'), {'format': 'txt'})
    W('export-file', 'POST export-file txt again', 'POST', note_path('cloud_export', 'export-file'), {'format': 'txt'})
    W('export-file', 'POST export-file txt same title A', 'POST', note_path('cloud_export_twice', 'export-file'), {'format': 'txt'})
    W('export-file', 'POST export-file txt empty title', 'POST', note_path('local_empty', 'export-file'), {'format': 'txt'})
    W('export-file', 'POST export-file bad format', 'POST', note_path('cloud_export', 'export-file'), {'format': 'docx'})
    W('export-file', 'POST export-file extra key', 'POST', note_path('cloud_export', 'export-file'), {'format': 'txt', 'x': 1})
    W('export-file', 'POST export-file pdf no pages', 'POST', note_path('cloud_plain', 'export-file'), {'format': 'pdf'})
    W('export-file', 'POST export-file pdf canvas', 'POST', note_path('canvas', 'export-file'), {'format': 'pdf'})
    W('export-file', 'POST export-file unknown note', 'POST', '/api/notes/00000000-0000-4000-8000-0000000fffff/export-file', {'format': 'txt'})
    # -- resolve
    W('resolve', 'POST resolve bad strategy', 'POST', note_path('uncertain', 'resolve'), {'strategy': 'whatever'})
    W('resolve', 'POST resolve keep_local_copy', 'POST', note_path('uncertain', 'resolve'), {'strategy': 'keep_local_copy'}, capture=capture_new('resolved_copy'))
    observe('uncertain')
    W('resolve', 'POST resolve conflict use_remote offline', 'POST', note_path('conflict', 'resolve'), {'strategy': 'use_remote'})
    W('resolve', 'POST resolve conflict keep_local_copy', 'POST', note_path('conflict', 'resolve'), {'strategy': 'keep_local_copy'})
    observe('conflict')
    W('resolve', 'POST resolve org uncertain', 'POST', note_path('cloud_resolve_org', 'resolve'), {'strategy': 'use_remote'})
    W('resolve', 'POST resolve org bad strategy', 'POST', note_path('cloud_resolve_org', 'resolve'), {'strategy': 'keep_local_copy'})
    W('resolve', 'POST resolve no conflict', 'POST', note_path('local_plain', 'resolve'), {'strategy': 'keep_local_copy'})
    W('resolve', 'POST resolve no body', 'POST', note_path('local_plain', 'resolve'), None)
    # -- sync / auth
    W('sync', 'POST sync', 'POST', '/api/sync', {})
    W('sync', 'POST sync with data', 'POST', '/api/sync', {'force': True})
    W('auth', 'POST auth/login offline', 'POST', '/api/auth/login', {})
    W('auth', 'POST auth/login no body', 'POST', '/api/auth/login', None)
    W('auth', 'POST auth/disconnect', 'POST', '/api/auth/disconnect', {})
    W('auth', 'GET auth/login', 'GET', '/api/auth/login')
    lists()
    for query in ('q=%C3%A7', 'filter=all&q=yeni', 'folder=' + folders['empty']):
        W('observe', 'GET /api/notes?' + query[:40], 'GET', '/api/notes?' + query)
    # final per-note state of everything
    for role in sorted(scenario['notes']) + ['new1', 'new2', 'new3', 'new_editor', 'resolved_copy']:
        W('final', f'GET note {role}', 'GET', note_path(role), capture=capture_note(role))
        W('final', f'GET editor {role}', 'GET', note_path(role, 'editor'))
        W('final', f'GET ink {role}', 'GET', note_path(role, 'ink'))
    return steps


def uninit_write_steps(scenario):
    steps = []
    W = lambda name, method, path, body=None: steps.append(J('uninit', name, method, path, body=body))
    W('PATCH organize metadata_not_ready', 'PATCH', note_path('cloud_plain'), {'favorite': True})
    W('PATCH organize local', 'PATCH', note_path('local_plain'), {'favorite': True})
    W('POST folder metadata_not_ready', 'POST', '/api/folders', {'name': 'x'})
    W('POST folder invalid before ready', 'POST', '/api/folders', {'bad': 1})
    W('PATCH folder metadata_not_ready', 'PATCH', '/api/folders/abc', {'name': 'x'})
    W('POST folder resolve', 'POST', '/api/folders/abc/resolve', {'strategy': 'use_remote'})
    W('POST note', 'POST', '/api/notes', {'title': 'Klasörsüz yeni', 'body': '…'})
    W('GET status', 'GET', '/api/status')
    W('GET folders', 'GET', '/api/folders')
    W('GET notes', 'GET', '/api/notes')
    W('GET favorites', 'GET', '/api/notes?filter=favorites')
    return steps


def tiny_pdf(sizes):
    """Same construction as tests.test_cloud_pdf_pages.tiny_pdf (stdlib only)."""
    objects = ['<< /Type /Catalog /Pages 2 0 R >>',
               '<< /Type /Pages /Kids [%s] /Count %d >>' % (' '.join(f'{3 + i} 0 R' for i in range(len(sizes))), len(sizes))]
    objects += [f'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] >>' for w, h in sizes]
    out, offsets = b'%PDF-1.4\n', []
    for number, body in enumerate(objects, 1):
        offsets.append(len(out)); out += f'{number} 0 obj\n{body}\nendobj\n'.encode()
    xref = len(out)
    out += f'xref\n0 {len(objects) + 1}\n0000000000 65535 f \n'.encode()
    out += b''.join(f'{o:010d} 00000 n \n'.encode() for o in offsets)
    return out + f'trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode()


# ---------------------------------------------------------------------------
# Execution and comparison
# ---------------------------------------------------------------------------

def run_step(step, ctx):
    try:
        raw = step.build(ctx)
    except KeyError as error:
        return {'status': 'SKIP', 'code': None, 'headers': [], 'body': f'<<unresolved {error}>>'.encode()}, b''
    response = parse_response(exchange(ctx.port, raw, half_close=step.half_close))
    attempts = 1
    while step.poll and response.get('code') == 202 and attempts < 60:
        time.sleep(0.25)
        response = parse_response(exchange(ctx.port, raw, half_close=step.half_close))
        attempts += 1
    response['attempts'] = attempts
    return response, raw


def compact_diff(a, b, limit=400):
    if a == b:
        return None
    raw = lambda v: v if isinstance(v, bytes) else str(v).encode('utf-8', 'surrogateescape')
    ra, rb = raw(a), raw(b)
    start = next((i for i, (x, y) in enumerate(zip(ra, rb)) if x != y), min(len(ra), len(rb)))
    try:
        ta, tb = ra.decode('utf-8'), rb.decode('utf-8')
    except UnicodeDecodeError:
        return {'binary': True, 'firstDiffByte': start, 'refLen': len(ra), 'candLen': len(rb),
                'refSha256': hashlib.sha256(ra).hexdigest(), 'candSha256': hashlib.sha256(rb).hexdigest()}
    start = next((i for i, (x, y) in enumerate(zip(ta, tb)) if x != y), min(len(ta), len(tb)))
    lo = max(0, start - 60)
    return {'at': start, 'ref': ta[lo:lo + limit], 'cand': tb[lo:lo + limit], 'refLen': len(ta), 'candLen': len(tb)}


def png_geometry(body):
    """IHDR width, height, bit depth, colour type (None if not a PNG)."""
    if len(body) >= 26 and body[:8] == b'\x89PNG\r\n\x1a\n' and body[12:16] == b'IHDR':
        return body[16:26]
    return None


def compare(ref, cand, masker):
    strip = lambda r: [(k, v) for k, v in r['headers'] if k.lower() != 'date']
    if ref['status'] == cand['status'] and strip(ref) == strip(cand) and ref['body'] == cand['body']:
        return 'byte', None
    if HARNESS['loose'] and png_geometry(ref['body']) is not None:
        rest = lambda r: [(k, v) for k, v in strip(r) if k.lower() != 'content-length']
        if ref['status'] == cand['status'] and rest(ref) == rest(cand) and png_geometry(ref['body']) == png_geometry(cand['body']):
            return 'loose', None
    ra, rb = masked_view(ref, masker, 'A'), masked_view(cand, masker, 'B')
    # Learn id correspondences (numbered by first appearance) for later masking.
    ids_a, ids_b = masker.unknown_ids(ref['body'].decode('utf-8', 'surrogateescape'), 'A'), \
        masker.unknown_ids(cand['body'].decode('utf-8', 'surrogateescape'), 'B')
    if ra == rb:
        for x, y in zip(ids_b, ids_a):
            masker.learn(x, y)
        return 'masked', None
    diff = {}
    if ra[0] != rb[0]:
        diff['status'] = {'ref': ra[0], 'cand': rb[0]}
    if ra[1] != rb[1]:
        diff['headers'] = {'ref': [h for h in ra[1] if h not in rb[1]], 'cand': [h for h in rb[1] if h not in ra[1]]}
    if ra[2] != rb[2]:
        diff['body'] = compact_diff(ra[2], rb[2])
    return 'fail', diff


def dump_database(path, masker, side):
    out = {}
    if not path.exists():
        return {'<missing>': []}
    db = sqlite3.connect(f'file:{path}?mode=ro', uri=True)
    try:
        tables = [r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
        out['<schema>'] = sorted(masker.text(r[0] or '', side, numbered=False) for r in db.execute('SELECT sql FROM sqlite_master'))
        for table in tables:
            rows = []
            for row in db.execute(f'SELECT * FROM "{table}"'):
                values = []
                for value in row:
                    if isinstance(value, str):
                        value = masker.text(value, side, numbered=False)
                    elif isinstance(value, bytes):
                        value = 'blob:' + hashlib.sha256(value).hexdigest()
                    elif isinstance(value, int):
                        value = masker.text(str(value), side, numbered=False)
                        value = int(value) if value.lstrip('-').isdigit() else value
                    values.append(value)
                rows.append(json.dumps(values, ensure_ascii=False, sort_keys=True))
            out[table] = sorted(rows)
    finally:
        db.close()
    return out


SKIP_FILES = re.compile(r'(^|/)(library\.sqlite3(-wal|-shm|-journal)?|engine\.lock|runtime-[0-9a-f]+\.json|open-[0-9a-f]+\.html)$')


def normalise_file(rel, raw):
    if rel.endswith('.pdf'):
        raw = re.sub(rb'\(D:[0-9+\-Z\']{8,32}\)', b'(D:<date>)', raw)
        raw = re.sub(rb'<xmp:(CreateDate|ModifyDate|MetadataDate)>[^<]*</xmp:\1>', rb'<xmp:\1><date></xmp:\1>', raw)
        raw = re.sub(rb'/ID \[<[0-9a-fA-F]+> <[0-9a-fA-F]+>\]', b'/ID [<id> <id>]', raw)
        raw = re.sub(rb'uuid:[0-9a-f-]{36}', b'uuid:<uuid>', raw)
    return raw


def tree_files(root, masker, side):
    result = {}
    if not root.exists():
        return result
    for path in sorted(root.rglob('*')):
        rel = path.relative_to(root).as_posix()
        if SKIP_FILES.search(rel):
            continue
        info = path.lstat()
        key = masker.text(rel, side, numbered=False)
        if stat.S_ISDIR(info.st_mode):
            result[key] = f'dir {oct(info.st_mode & 0o777)}'
        elif stat.S_ISLNK(info.st_mode):
            result[key] = 'symlink'
        else:
            raw = normalise_file(rel, path.read_bytes())
            text = raw.decode('utf-8', 'surrogateescape')
            if rel.endswith(('.json', '.txt')):
                raw = masker.text(text, side, numbered=False).encode('utf-8', 'surrogateescape')
            result[key] = f'file {oct(info.st_mode & 0o777)} {len(raw)} {hashlib.sha256(raw).hexdigest()}'
    return result


def diff_maps(a, b, limit=8):
    problems = []
    for key in sorted(set(a) | set(b)):
        if a.get(key) != b.get(key):
            problems.append({'key': key, 'ref': a.get(key), 'cand': b.get(key)})
    return problems[:limit], len(problems)


def server_root(base, side):
    """``base/srv<side>-ppp…`` padded so the root string is ROOT_LENGTH long."""
    stem = str(base / f'srv{side}-')
    if len(stem) + 1 > ROOT_LENGTH:
        raise SystemExit(f'scratch path too long for ROOT_LENGTH={ROOT_LENGTH}: {base} (use a shorter --scratch)')
    return Path(stem + 'p' * (ROOT_LENGTH - len(stem)))


def golden_root():
    return GOLDEN_ROOT + 'x' * (ROOT_LENGTH - len(GOLDEN_ROOT))


def encode_response(response, root):
    """JSON-safe recorded response; the recording root is replaced by the
    same-length neutral GOLDEN_ROOT (no machine path in the repository)."""
    stand_in = golden_root()
    swap = lambda text: text.replace(root, stand_in)
    body = response['body'].replace(root.encode(), stand_in.encode())
    try:
        text, encoding = body.decode('utf-8'), 'utf-8'
    except UnicodeDecodeError:
        text, encoding = base64.b64encode(body).decode('ascii'), 'base64'
    return {'status': response.get('status'), 'code': response.get('code'),
            'headers': [[k, swap(v)] for k, v in response.get('headers', [])],
            'body': text, 'encoding': encoding, 'attempts': response.get('attempts')}


def decode_response(value):
    body = value['body'].encode('utf-8') if value['encoding'] == 'utf-8' else base64.b64decode(value['body'])
    return {'status': value['status'], 'code': value['code'], 'headers': [tuple(h) for h in value['headers']],
            'body': body, 'attempts': value.get('attempts')}


def write_gzip_json(path, value):
    import gzip
    raw = (json.dumps(value, indent=0, ensure_ascii=False, sort_keys=True) + '\n').encode()
    with open(path, 'wb') as handle:
        with gzip.GzipFile(filename='', mode='wb', fileobj=handle, mtime=0, compresslevel=9) as stream:
            stream.write(raw)


def read_gzip_json(path):
    import gzip
    with gzip.open(path, 'rb') as stream:
        return json.loads(stream.read())


def state_dumps(layout, masker, side):
    db = dump_database(layout['data'] / 'library.sqlite3', masker, side)
    files = {**{'data/' + k: v for k, v in tree_files(layout['data'], masker, side).items()},
             **{'Downloads/' + k: v for k, v in tree_files(layout['downloads'], masker, side).items()}}
    return db, files


def run_scenario(name, scenario, library_dir, args, scratch, report, mode='live', golden=None):
    """mode: 'live' (reference + candidate servers), 'record' (reference only;
    returns the golden recording) or 'replay' (recorded reference + candidate)."""
    base = scratch / name
    use_ref, use_cand = mode in ('live', 'record'), mode in ('live', 'replay')
    empty = (golden or {}).get('emptyDirs', ())
    layout_a = make_server_root(server_root(base, 'A'), library_dir) if use_ref else None
    layout_b = make_server_root(server_root(base, 'B'), library_dir, empty) if use_cand else None
    known = set()
    # immutable=1: never create -wal/-shm next to the (committed) input library.
    db = sqlite3.connect(f'file:{library_dir / "library.sqlite3"}?mode=ro&immutable=1', uri=True)
    try:
        for (table,) in db.execute("SELECT name FROM sqlite_master WHERE type='table'"):
            for row in db.execute(f'SELECT * FROM "{table}"'):
                for value in row:
                    if isinstance(value, str):
                        known.update(UUID_RE.findall(value))
    finally:
        db.close()
    known.update(v for v in json.dumps(scenario).split('"') if UUID_RE.fullmatch(v))
    start_ms = time.time_ns() // 1_000_000
    window = (start_ms - 120_000, start_ms + 3_600_000)
    proc_a = proc_b = None
    results, recorded = [], []
    code_a = code_b = None
    try:
        if use_ref:
            proc_a, runtime_a = start_server(args.reference, layout_a, base / 'reference.log')
            root_a, window_a = str(layout_a['root']), window
        else:
            runtime_a = {'port': golden['port'], 'token': 'recorded', 'pid': 0}
            root_a, window_a = golden['root'], tuple(golden['window'])
        if use_cand:
            proc_b, runtime_b = start_server(args.candidate, layout_b, base / 'candidate.log')
            root_b = str(layout_b['root'])
        else:
            runtime_b, root_b = {'port': 0, 'token': '', 'pid': 0}, '\0unused'
        ctx_a = Ctx('reference', runtime_a, layout_a, scenario)
        ctx_b = Ctx('candidate', runtime_b, layout_b, scenario)
        masker = Masker(known, {'A': window_a, 'B': window}, {'A': root_a, 'B': root_b},
                        {'A': runtime_a['port'], 'B': runtime_b['port']})
        if scenario.get('folderInitialized'):
            steps = read_steps(scenario, True) + write_steps(scenario)
        else:
            steps = read_steps(scenario, False) + uninit_write_steps(scenario)
        if mode == 'replay':
            names = [(s.category, s.name) for s in steps]
            stored = [(r['category'], r['name']) for r in golden['steps']]
            if names != stored:
                raise RuntimeError(f'golden recording does not match the current step list ({len(stored)} recorded, '
                                   f'{len(names)} now; first difference at '
                                   f'{next((i for i, (x, y) in enumerate(zip(names, stored)) if x != y), min(len(names), len(stored)))}). '
                                   'Re-record with --record (needs the Python reference).')
        for index, step in enumerate(steps):
            if mode == 'replay':
                ref = decode_response(golden['steps'][index]['response'])
            else:
                ref, _ = run_step(step, ctx_a)
            cand = run_step(step, ctx_b)[0] if use_cand else None
            if step.capture:
                for ctx, response in ((ctx_a, ref), (ctx_b, cand)):
                    if response is not None and response.get('status') != 'SKIP':
                        try:
                            step.capture(ctx, response)
                        except (KeyError, TypeError, AttributeError):
                            pass
                for key in set(ctx_a.vars) & set(ctx_b.vars):
                    masker.learn(ctx_b.vars[key], ctx_a.vars[key])
            if mode == 'record':
                if ref.get('status') == 'SKIP':
                    raise RuntimeError(f'reference could not build step {index} {step.name}')
                recorded.append({'index': index, 'category': step.category, 'name': step.name,
                                 'response': encode_response(ref, root_a)})
                continue
            if ref.get('status') == 'SKIP' and cand.get('status') == 'SKIP':
                verdict, diff = 'skip', None
            else:
                verdict, diff = compare(ref, cand, masker)
            entry = {'scenario': name, 'index': index, 'category': step.category, 'name': step.name,
                     'verdict': verdict, 'status': ref.get('status'), 'attempts': [ref.get('attempts'), cand.get('attempts')]}
            if diff:
                entry['diff'] = diff
            results.append(entry)
            if verdict == 'fail' or args.verbose:
                label = {'byte': 'PASS', 'masked': 'PASS*', 'loose': 'PASS~', 'fail': 'FAIL', 'skip': 'SKIP'}[verdict]
                print(f'{label:5} [{name}/{step.category}] {step.name} -> {ref.get("status")}')
                if diff:
                    print('      ' + json.dumps(diff, ensure_ascii=False)[:1200])
    finally:
        code_a = stop_server(proc_a) if proc_a else None
        code_b = stop_server(proc_b) if proc_b else None
    if mode == 'replay':
        db_a, files_a = golden['state']['database'], golden['state']['files']
        code_a = golden['state']['exit']
    else:
        db_a, files_a = state_dumps(layout_a, masker, 'A')
    if mode == 'record':
        print(f'REC   [{name}] {len(recorded)} responses, {len(db_a)} tables, {len(files_a)} files; reference exit {code_a}')
        empty_dirs = sorted(p.relative_to(library_dir).as_posix() for p in library_dir.rglob('*')
                            if p.is_dir() and not p.is_symlink() and not any(p.iterdir()))
        return {'schemaVersion': 1, 'scenario': name, 'root': golden_root(), 'port': runtime_a['port'],
                'emptyDirs': empty_dirs,
                'window': list(window_a), 'steps': recorded,
                'state': {'exit': code_a, 'database': db_a, 'files': files_a}}
    state = {'exit': {'reference': code_a, 'candidate': code_b}}
    db_b, files_b = state_dumps(layout_b, masker, 'B')
    db_problems = []
    for table in sorted(set(db_a) | set(db_b)):
        rows_a, rows_b = db_a.get(table), db_b.get(table)
        if rows_a != rows_b:
            only_a = sorted(set(rows_a or []) - set(rows_b or []))
            only_b = sorted(set(rows_b or []) - set(rows_a or []))
            db_problems.append({'table': table, 'refRows': len(rows_a or []), 'candRows': len(rows_b or []),
                                'onlyRef': [r[:300] for r in only_a[:3]], 'onlyCand': [r[:300] for r in only_b[:3]]})
    if HARNESS['loose']:
        # Renderer output files: same path and mode, bytes not compared.
        rendered = re.compile(r'(\.png$|^Downloads/.*\.pdf$)')
        shape = lambda files: {k: (' '.join(v.split()[:2]) if rendered.search(k) and v.startswith('file ') else v)
                               for k, v in files.items()}
        files_a, files_b = shape(files_a), shape(files_b)
    file_problems, file_count = diff_maps(files_a, files_b)
    state.update(database={'tables': len(db_a), 'mismatches': db_problems},
                 files={'count': len(files_a), 'mismatches': file_problems, 'mismatchCount': file_count})
    for problem in db_problems:
        print(f'FAIL  [{name}/sqlite] table {problem["table"]}: ' + json.dumps(problem, ensure_ascii=False)[:1200])
    for problem in file_problems:
        print(f'FAIL  [{name}/files] ' + json.dumps(problem, ensure_ascii=False)[:600])
    if code_a not in (0, None) or code_b not in (0, None):
        print(f'NOTE  [{name}] exit codes reference={code_a} candidate={code_b}')
    report['scenarios'][name] = {'requests': results, 'state': state}
    return results, state


def available_tools():
    tools = {name: shutil.which(name, path=HARNESS['path']) is not None
             for name in ('pdfinfo', 'pdftoppm', 'bwrap', 'magick', 'xdg-user-dir')}
    tools['ink-render'] = any((ROOT / 'tools/ink-renderer/target' / kind / 'ink-render').is_file()
                              for kind in ('release', 'debug'))
    return tools


def tool_versions():
    """Version identity of the tools whose output bytes end up in responses."""
    def first_line(argv):
        exe = shutil.which(argv[0], path=HARNESS['path'])
        if not exe:
            return None
        try:
            out = subprocess.run([exe, *argv[1:]], capture_output=True, text=True, timeout=20)
        except (OSError, subprocess.TimeoutExpired):
            return None
        lines = (out.stdout + out.stderr).strip().splitlines()
        return lines[0].strip() if lines else ''
    versions = {'pdftoppm': first_line(['pdftoppm', '-v']), 'rsvg-convert': first_line(['rsvg-convert', '--version']),
                'magick': first_line(['magick', '-version']), 'bwrap': first_line(['bwrap', '--version'])}
    renderer = next((ROOT / 'tools/ink-renderer/target' / kind / 'ink-render' for kind in ('release', 'debug')
                     if (ROOT / 'tools/ink-renderer/target' / kind / 'ink-render').is_file()), None)
    versions['ink-render'] = hashlib.sha256(renderer.read_bytes()).hexdigest() if renderer else None
    return versions


def tool_farm(hidden, parent):
    """A PATH directory with every executable of PATH except ``hidden``."""
    farm = Path(tempfile.mkdtemp(prefix='path-', dir=parent))
    for directory in os.environ.get('PATH', '/usr/bin:/bin').split(os.pathsep):
        try:
            entries = sorted(os.listdir(directory or '.'))
        except OSError:
            continue
        for name in entries:
            source = Path(directory) / name
            if name in hidden or (farm / name).exists() or (farm / name).is_symlink():
                continue
            if source.is_file() and os.access(source, os.X_OK):
                (farm / name).symlink_to(source)
    return str(farm)


def record(args, library, manifest, names):
    """Run the Python reference once over the synthetic library and freeze its
    responses and final state as golden fixtures (synthetic data only)."""
    out = args.record.resolve()
    tools = available_tools()
    hidden = set(args.hide_tools)
    missing = [name for name, present in tools.items() if not present and name not in hidden]
    if missing:
        raise SystemExit('refusing to record without every optional tool (goldens must cover the full routes): '
                         + ', '.join(missing))
    if bool(args.variant) != bool(hidden):
        raise SystemExit('--variant NAME and --hide-tools go together (a variant records with some tools hidden)')
    parent = args.scratch.resolve() if args.scratch else Path(tempfile.gettempdir())
    parent.mkdir(parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix='rec-', dir=parent))
    recordings = {}
    try:
        for name in names:
            recordings[name] = run_scenario(name, manifest['scenarios'][name], library / name, args, scratch,
                                            {'scenarios': {}}, mode='record')
    finally:
        if not args.keep:
            shutil.rmtree(scratch, ignore_errors=True)
    # Nothing machine-specific may leak into the repository.
    leaks = [str(ROOT), str(Path.home()), str(scratch)]
    for name, value in recordings.items():
        blob = json.dumps(value, ensure_ascii=False)
        for leak in leaks:
            if leak and leak in blob:
                raise SystemExit(f'recording {name} contains a machine path ({leak!r}); not written')
    versions = tool_versions()
    if args.variant:
        meta_path = out / 'golden.json'
        meta = json.loads(meta_path.read_text())
        target = out / 'variants' / args.variant
        target.mkdir(parents=True, exist_ok=True)
        for name, value in recordings.items():
            write_gzip_json(target / f'{name}.json.gz', value)
        meta.setdefault('variants', {})[args.variant] = {
            'hidden': sorted(hidden), 'tools': tools, 'toolVersions': versions,
            'scenarios': {name: {'file': f'variants/{args.variant}/{name}.json.gz', 'responses': len(value['steps'])}
                          for name, value in recordings.items()}}
        meta_path.write_text(json.dumps(meta, indent=1, ensure_ascii=False, sort_keys=True) + '\n')
        print(f'\n== recorded variant {args.variant} (hidden: {", ".join(sorted(hidden))}) into {target}')
        return 0
    staging = out.with_name(out.name + '.new')
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)
    shutil.copytree(library, staging / 'library', symlinks=True,
                    ignore=shutil.ignore_patterns('*.sqlite3-wal', '*.sqlite3-shm', '*.sqlite3-journal'))
    for name, value in recordings.items():
        write_gzip_json(staging / f'{name}.json.gz', value)
    meta = {'schemaVersion': 1, 'reference': args.reference, 'rootLength': ROOT_LENGTH,
            'recordedOn': time.strftime('%Y-%m-%d', time.gmtime()), 'tools': tools, 'toolVersions': versions,
            'scenarios': {name: {'file': f'{name}.json.gz', 'responses': len(value['steps']),
                                 'tables': len(value['state']['database']), 'files': len(value['state']['files'])}
                          for name, value in recordings.items()},
            'note': 'Frozen responses of the historical Python reference (NOTES_IMPL=rust python3 -m samsung_notes) '
                    'over the synthetic library in library/ (reference/python/conformance/gen_library.py). '
                    'Synthetic data only. Replay: run.py --replay --candidate target/debug/notes-engine'}
    (staging / 'golden.json').write_text(json.dumps(meta, indent=1, ensure_ascii=False, sort_keys=True) + '\n')
    if out.exists():
        old = out.with_name(out.name + '.old')
        shutil.rmtree(old, ignore_errors=True)
        out.rename(old)
    staging.rename(out)
    total = sum(len(v['steps']) for v in recordings.values())
    print(f'\n== recorded {total} reference responses for {len(recordings)} scenarios into {out}'
          ' (variants of the previous goldens were not carried over; re-record them with --variant)')
    return 0


def main():
    parser = argparse.ArgumentParser(description='Byte-level HTTP conformance: reference vs candidate engine.')
    parser.add_argument('--library', type=Path, help='output directory of gen_library.py (replay default: GOLDEN/library)')
    parser.add_argument('--candidate', help='candidate command prefix')
    parser.add_argument('--reference', default=DEFAULT_REFERENCE, help=f'reference command prefix (default: {DEFAULT_REFERENCE})')
    parser.add_argument('--record', type=Path, nargs='?', const=GOLDEN, metavar='DIR',
                        help=f'run only the reference and freeze golden fixtures into DIR (default {GOLDEN})')
    parser.add_argument('--replay', type=Path, nargs='?', const=GOLDEN, metavar='DIR',
                        help=f'compare the candidate against golden fixtures in DIR (default {GOLDEN}); no reference, no Python package')
    parser.add_argument('--hide-tools', default='', help='comma-separated tools hidden from the servers\' PATH (e.g. magick)')
    parser.add_argument('--variant', help='with --record and --hide-tools: store as this golden variant (e.g. no-magick)')
    parser.add_argument('--report', type=Path, help='write a JSON report here')
    parser.add_argument('--scratch', type=Path, help='scratch parent directory (default: a new temp dir)')
    parser.add_argument('--keep', action='store_true', help='keep the scratch copies and logs')
    parser.add_argument('--scenario', help='comma-separated scenario names (default: all)')
    parser.add_argument('--repeat', type=int, default=1, help='run everything N times (flakiness check)')
    parser.add_argument('-v', '--verbose', action='store_true', help='print passing requests too')
    args = parser.parse_args()
    if args.record and args.replay:
        parser.error('--record and --replay are exclusive')
    if not args.record and not args.candidate:
        parser.error('--candidate is required (except with --record)')
    golden_dir = args.replay.resolve() if args.replay else None
    if args.library is None:
        if not golden_dir:
            parser.error('--library is required (except with --replay)')
        args.library = golden_dir / 'library'
    library = args.library.resolve()
    manifest = json.loads((library / 'manifest.json').read_text())
    names = args.scenario.split(',') if args.scenario else sorted(manifest['scenarios'])
    os.umask(0o077)
    args.hide_tools = [t for t in args.hide_tools.split(',') if t]
    if args.hide_tools:
        farm_parent = args.scratch.resolve() if args.scratch else Path(tempfile.gettempdir())
        farm_parent.mkdir(parents=True, exist_ok=True)
        HARNESS['path'] = tool_farm(set(args.hide_tools), farm_parent)
        atexit.register(shutil.rmtree, HARNESS['path'], True)
    if args.record:
        return record(args, library, manifest, names)
    goldens = {}
    tools = available_tools()
    if golden_dir:
        meta = json.loads((golden_dir / 'golden.json').read_text())
        if meta.get('rootLength') != ROOT_LENGTH:
            raise SystemExit(f'golden rootLength {meta.get("rootLength")} != harness ROOT_LENGTH {ROOT_LENGTH}; re-record')
        # Pick the recording whose hidden tools are exactly the ones missing here.
        absent = {k for k, v in meta['tools'].items() if v and not tools.get(k)}
        profile, label = meta, 'full'
        for vname, variant in sorted(meta.get('variants', {}).items()):
            if absent and set(variant['hidden']) == absent:
                profile, label = {**meta, **variant}, vname
        for name in names:
            goldens[name] = read_gzip_json(golden_dir / profile['scenarios'][name]['file'])
        differ = [k for k, v in profile['tools'].items() if tools.get(k) != v]
        if differ:
            print('NOTE  optional tools differ from the recording environment (their routes will differ): '
                  + ', '.join(f'{k} recorded={profile["tools"][k]} here={tools.get(k)}' for k in differ))
        here = tool_versions()
        drift = [t for t in RENDER_TOOLS if profile.get('toolVersions', {}).get(t) is not None
                 and here.get(t) is not None and profile['toolVersions'][t] != here.get(t)]
        if drift:
            HARNESS['loose'] = True
            print('NOTE  renderer versions differ from the recording (' + ', '.join(drift) + '): PNG responses are '
                  'compared by status/headers/IHDR geometry and rendered files by path/mode (PASS~ = loose-equal)')
        args.reference = (f'golden:{golden_dir.name}/{label} ({meta.get("reference")}, {meta.get("recordedOn")})')
    missing = [name for name, present in tools.items() if not present]
    if missing:
        print('NOTE  missing optional tools (both servers see the same environment, but the routes using them '
              'only exercise their error paths): ' + ', '.join(missing))
    overall = 0
    reports = []
    for attempt in range(args.repeat):
        parent = args.scratch.resolve() if args.scratch else Path(tempfile.gettempdir())
        parent.mkdir(parents=True, exist_ok=True)
        scratch = Path(tempfile.mkdtemp(prefix='run-', dir=parent))
        report = {'schemaVersion': 1, 'reference': args.reference, 'candidate': args.candidate, 'library': str(library),
                  'attempt': attempt + 1, 'tools': tools, 'scenarios': {}}
        totals = {'requests': 0, 'byte': 0, 'masked': 0, 'loose': 0, 'fail': 0, 'skip': 0, 'stateFailures': 0}
        categories = {}
        try:
            for name in names:
                try:
                    results, state = run_scenario(name, manifest['scenarios'][name], library / name, args, scratch, report,
                                                  mode='replay' if golden_dir else 'live', golden=goldens.get(name))
                except RuntimeError as error:
                    print(f'FAIL  [{name}] could not run scenario: {error}')
                    report['scenarios'][name] = {'error': str(error)}
                    totals['stateFailures'] += 1
                    continue
                for entry in results:
                    totals['requests'] += 1
                    totals[entry['verdict']] += 1
                    bucket = categories.setdefault(entry['category'], {'requests': 0, 'byte': 0, 'masked': 0, 'loose': 0, 'fail': 0, 'skip': 0})
                    bucket['requests'] += 1; bucket[entry['verdict']] += 1
                totals['stateFailures'] += len(state['database']['mismatches']) + state['files']['mismatchCount']
        finally:
            if not args.keep:
                shutil.rmtree(scratch, ignore_errors=True)
        report['totals'], report['categories'] = totals, categories
        ok = totals['fail'] == 0 and totals['stateFailures'] == 0 and totals['skip'] == 0
        report['passed'] = ok
        reports.append(report)
        print(f'\n== attempt {attempt + 1}/{args.repeat}{" (replay)" if golden_dir else ""}: {totals["byte"]}/{totals["requests"]} byte-equal, '
              f'{totals["masked"]} masked-equal, ' + (f'{totals["loose"]} loose-equal (renderer drift), ' if totals['loose'] else '')
              + f'{totals["fail"]} failed, {totals["skip"]} skipped; '
              f'state mismatches: {totals["stateFailures"]}  => {"PASS" if ok else "FAIL"}')
        if args.keep:
            print(f'   scratch kept at {scratch}')
        overall = overall or (0 if ok else 1)
    if args.report:
        value = reports[0] if len(reports) == 1 else {'schemaVersion': 1, 'runs': reports}
        args.report.write_text(json.dumps(value, indent=1, ensure_ascii=False) + '\n')
    return overall


if __name__ == '__main__':
    raise SystemExit(main())

#!/usr/bin/env python3
"""Build the fuzz seed corpora in fuzz/corpus/<target>/ from committed,
synthetic test fixtures only (never a user's library):

* crates/notes-host/tests/fixtures/cases.json      (synthetic snapshots, models, points)
* crates/notes-cloud/tests/fixtures/oracle/*.json.gz (recorded synthetic Cloud exchanges)
* crates/notes-protocol/tests/fixtures/            (change feed, note tree, sign-in JSON)
* crates/notes-preview/tests/fixtures/spi/*.spi    (synthetic canvas caches)
* crates/notes-engine/conformance/golden/library/*/page-previews|thumbnails (*.png)
* fuzz/regressions/<target>/                       (minimised crash inputs)

plus a few hand-written HTTP requests/responses and callback URIs.
Usage: python3 fuzz/make_seeds.py   (idempotent; files are named by SHA-1)
"""
import base64
import glob
import gzip
import hashlib
import json
import os
import struct
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "fuzz", "corpus")
TARGETS = ["payload", "document", "xml", "cloud_response", "ink_binary", "json", "api_values",
           "http_response", "login", "local_api", "png", "spi"]
STROKE_WRAPPER = b"\xff\xff\xff\xff\x04\x00\x00\x00\xa0\x0f\x00\x00"


def put(target, data):
    if isinstance(data, str):
        data = data.encode()
    if not data or len(data) > 1 << 20:
        return
    d = os.path.join(OUT, target)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, hashlib.sha1(data).hexdigest()), "wb") as f:
        f.write(data)


def fixture(*parts):
    return os.path.join(ROOT, "crates", *parts)


def boundary_of(raw):
    if raw.startswith(b"--"):
        line = raw.split(b"\r\n", 1)[0][2:]
        if 0 < len(line) < 200:
            return "multipart/form-data; boundary=" + line.decode("latin-1")
    return None


def strokes(raw):
    out, at = [], raw.find(STROKE_WRAPPER)
    while at >= 0:
        end = raw.find(b"\r\n--", at)
        out.append(raw[at:end if end > 0 else len(raw)])
        at = raw.find(STROKE_WRAPPER, at + 1)
    return out


def from_cases():
    cases = json.load(open(fixture("notes-host", "tests", "fixtures", "cases.json")))
    for doc in cases["docs"].values():
        if not isinstance(doc, dict) or "raw_body" not in doc:
            continue
        raw = base64.b64decode(doc["raw_body"])
        ctype = doc.get("content_type", "")
        for t in ("payload", "document"):
            put(t, ctype.encode() + b"\n" + raw)
        xml = base64.b64decode(doc.get("resource_xml", ""))
        put("xml", xml)
        for sel in range(4):
            put("cloud_response", bytes([sel]) + xml)
        for s in strokes(raw):
            for sel in (0, 1, 2, 3, 7):
                put("ink_binary", bytes([sel]) + s)
            put("ink_binary", bytes([5]) + s[12:])
        text = json.dumps(doc)
        put("json", text)
        put("api_values", b"\x07" + text.encode())
        put("api_values", b"\x06" + text.encode())
    for model in cases["models"]:
        put("json", json.dumps(model))
        put("api_values", b"\x02" + json.dumps(model).encode())
    for points in cases["points"]:
        put("json", json.dumps(points))
        put("api_values", b"\x03" + json.dumps(points).encode())
        put("api_values", b"\x03" + json.dumps({"x": 1, "points": points}).encode())


def from_oracle():
    for path in glob.glob(fixture("notes-cloud", "tests", "fixtures", "oracle", "*.json.gz")):
        record = json.load(gzip.open(path))
        for step in record["steps"]:
            for exchange in step.get("exchanges", []):
                response = exchange.get("response") or {}
                raw = response.get("raw")
                if not isinstance(raw, str):
                    continue
                try:
                    body = base64.b64decode(raw)
                except ValueError:
                    continue
                name = exchange.get("name", "")
                ctype = boundary_of(body)
                if ctype:
                    for t in ("payload", "document"):
                        put(t, ctype.encode() + b"\n" + body)
                    for s in strokes(body):
                        put("ink_binary", b"\x00" + s)
                elif body.lstrip().startswith(b"<"):
                    put("xml", body)
                    sel = {"compare": 0, "publish": 4, "create-upload-urls": 5, "changes": 7}.get(name)
                    for s in ([sel] if sel is not None else range(8)):
                        put("cloud_response", bytes([s]) + body)
                    if name == "compare":
                        for s in (1, 2, 3):
                            put("cloud_response", bytes([s]) + body)
                elif body.lstrip().startswith((b"{", b"[")):
                    put("json", body)
                    put("cloud_response", b"\x06" + body)
                # The same body as an HTTP response, gzip and chunked.
                status = response.get("status") or 200
                head = b"HTTP/1.1 %d OK\r\nContent-Type: application/xml\r\n" % status
                put("http_response", b"\x01" + head + b"Content-Length: %d\r\n\r\n" % len(body) + body)
                gz = gzip.compress(body, mtime=0)
                put("http_response", b"\x05" + head + b"Content-Encoding: gzip\r\nContent-Length: %d\r\n\r\n" % len(gz) + gz)
                chunked = b"".join(b"%x\r\n%s\r\n" % (len(body[i:i + 300]), body[i:i + 300]) for i in range(0, len(body), 300))
                put("http_response", b"\x09" + head + b"Transfer-Encoding: chunked\r\n\r\n" + chunked + b"0\r\nX-T: 1\r\n\r\n")


def from_protocol():
    base = fixture("notes-protocol", "tests", "fixtures")
    for name in ("changes.xml", "tree.xml"):
        data = open(os.path.join(base, name), "rb").read()
        put("xml", data)
        put("cloud_response", b"\x07" + data)
    for path in glob.glob(os.path.join(base, "**", "*"), recursive=True):
        if os.path.isfile(path) and path.endswith(".json"):
            put("json", open(path, "rb").read())
    login_profile = fixture("notes-login", "src", "login_profile.json")
    if os.path.exists(login_profile):
        put("json", open(login_profile, "rb").read())


def from_images():
    for path in sorted(glob.glob(fixture("notes-preview", "tests", "fixtures", "spi", "*.spi"))):
        data = open(path, "rb").read()
        put("spi", data)
        put("ink_binary", b"\x00" + data)
    pngs = glob.glob(fixture("notes-engine", "conformance", "golden", "library", "*", "page-previews", "*.png"))
    pngs += glob.glob(fixture("notes-engine", "conformance", "golden", "library", "*", "thumbnails", "**", "*.png"), recursive=True)
    for path in sorted(pngs)[:40]:
        data = open(path, "rb").read()
        if data[12:16] == b"IHDR":
            w, h = struct.unpack(">II", data[16:24])
            put("png", bytes([min(w, 256) - 1, min(h, 256) - 1]) + data)
    # A tiny valid RGBA8 PNG (3x2) the decoder accepts exactly.
    w, h = 3, 2
    raw = b"".join(b"\x00" + bytes(range(4 * w)) for _ in range(h))

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + \
        chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
    put("png", bytes([w - 1, h - 1]) + png)
    put("png", bytes([w - 1, h - 1]) + b"P6\n3 2\n255\n" + bytes(18))


def handwritten():
    for body in [b"{}", b"[]", b'{"a": [1, 2.5, -3e7, true, null, "\\u00e7\\ud83d\\ude00"]}', b'"x"',
                 b'{"schemaVersion": 1, "keys": {"a": 1}}', b"\xef\xbb\xbf{\"bom\": 1}", b"NaN", b"[Infinity]"]:
        put("json", body)
    for sel, body in [(0, {"theme": "system", "zoom": 100, "view": "grid"}), (0, {}),
                      (1, {"schemaVersion": 2, "bindings": {"newNote": "Ctrl+N", "searchNotes": "Ctrl+K"}}),
                      (4, {"kind": "rectangle", "geometry": [10, 10, 200, 100], "colorARGB": -16777216, "width": 2.0, "fillARGB": None}),
                      (4, {"kind": "arrow", "geometry": [0, 0, 50, 50], "colorARGB": 4278190080, "width": 1}),
                      (5, {"title": "Başlık", "body": "Gövde"}), (5, {"title": "x" * 300, "body": None})]:
        put("api_values", bytes([sel]) + json.dumps(body).encode())
    responses = [
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello",
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\nX: y\r\n\r\n",
        b"HTTP/1.0 302 Found\r\nLocation: https://[::1]:443/x\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;ext=1\r\nhello\r\n0\r\n\r\n",
        b"HTTP/1.1 404 Not Found\r\nContent-Type: application/json; charset=utf-8\r\n folded\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n" + gzip.compress(b"x" * 4096, mtime=0) + gzip.compress(b"second member", mtime=0),
    ]
    for r in responses:
        for sel in (0, 1, 2, 7):
            put("http_response", bytes([sel]) + r)
    uri = "ms-app://s-1-15-2-synthetic/?code=Syn123&state=SyntheticStateSyntheticStateSyntheticState0"
    for sel, text in [(0, uri), (0, uri.replace("code=Syn123", "error=access_denied")), (1, uri),
                      (1, "https://user:pw@[::1]:8080/p;q?a=1&b=%C3%A7+x#f"), (2, "00" * 16 + "\nab\ncd\nef"),
                      (3, "/api/notes?q=%C3%A7&filter=all&folder=x"), (4, "https://eu1rgn-down.file.samsungcloud.com/x?y=1"),
                      (5, "/usr/bin/notes engine %u")]:
        put("login", bytes([sel]) + text.encode())
    note = "0b2b0c9e-7a8c-4c47-9a1d-3f5d2c8b9e10"
    requests = [
        "GET /api/status\n\r\n",
        "GET /api/templates?note=%s\n\r\n" % note,
        "GET /api/preferences\n\r\n",
        "GET /api/notes?q=a&filter=all\n\r\n",
        "GET /api/folders\n\r\n",
        "GET /api/notes/%s\n\r\n" % note,
        "GET /api/notes/%s/preview?page=0\n\r\n" % note,
        "GET /api/notes/%s/thumbnail?revision=3\n\r\n" % note,
        "GET /api/notes/%s/ink-geometry?page=%s&revision=2\n\r\n" % (note, note),
        "GET /api/notes/%s/ink-pens?revision=2\n\r\n" % note,
        "GET /api/notes/%s/export\n\r\n" % note,
        "GET /\n\r\n", "GET /assets/app.js\n\r\n", "HEAD /index.html\n\r\n", "GET /local-fonts/x.ttf\n\r\n",
    ]
    bodies = [
        ("PUT /api/preferences", {"theme": "dark"}),
        ("PUT /api/shortcuts", {"schemaVersion": 2, "bindings": {"newNote": "Ctrl+N"}}),
        ("POST /api/folders", {"name": "Klasör", "parentId": None}),
        ("PATCH /api/folders/abc", {"name": "Yeni"}),
        ("POST /api/folders/abc/resolve", {"strategy": "keep_local"}),
        ("POST /api/notes/editor", {"title": "T", "model": {"schemaVersion": 1, "text": "A", "spans": [], "paragraphs": []}}),
        ("POST /api/notes", {"title": "T", "body": "B"}),
        ("PUT /api/notes/%s" % note, {"title": "T", "body": "B", "expectedRevision": 2}),
        ("PATCH /api/notes/%s" % note, {"favorite": True}),
        ("PUT /api/notes/%s/ink" % note, {"expectedRevision": 2, "strokes": [{"pageId": note, "sourceStrokeId": note,
                                                                              "points": [{"x": 1, "y": 2, "pressure": 0.5, "timeMs": 0}]}]}),
        ("PUT /api/notes/%s/ink-erase" % note, {"erase": [note], "expectedRevision": 2}),
        ("POST /api/notes/%s/page-ops" % note, {"op": "add", "afterIndex": 0, "expectedRevision": 2}),
        ("PUT /api/notes/%s/page-style" % note, {"backgroundArgb": -1, "expectedRevision": 2}),
        ("PUT /api/notes/%s/editor" % note, {"title": "T", "model": {"schemaVersion": 1, "text": "", "spans": [], "paragraphs": []},
                                             "expectedRevision": 2}),
        ("POST /api/notes/%s/export-file" % note, {"format": "pdf"}),
        ("POST /api/sync", {}),
    ]
    for line in requests:
        put("local_api", b"\x01" + line.encode())
    for line, body in bodies:
        data = json.dumps(body).encode()
        put("local_api", b"\x01" + line.encode() + b"\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n" % len(data) + data)
    pdf = b"%PDF-1.4\n%\xe2\xe3\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF"
    put("local_api", b"\x01POST /api/notes/%s/pdf?revision=2&name=a.pdf\nContent-Type: application/pdf\r\nContent-Length: %d\r\n\r\n" % (note.encode(), len(pdf)) + pdf)
    put("local_api", b"\x00GET /api/status HTTP/1.1\r\nHost: 127.0.0.1:8123\r\nOrigin: http://127.0.0.1:8123\r\n\r\n")
    put("local_api", b"\x00GET /\r\n")
    put("local_api", b"\x00BREW /x HTTP/1.1\r\n\r\n")


def regressions():
    for target in TARGETS:
        for path in glob.glob(os.path.join(ROOT, "fuzz", "regressions", target, "*")):
            put(target, open(path, "rb").read())


def main():
    from_cases()
    from_oracle()
    from_protocol()
    from_images()
    handwritten()
    regressions()
    for target in TARGETS:
        d = os.path.join(OUT, target)
        print(f"{target:15s} {len(os.listdir(d)) if os.path.isdir(d) else 0:5d} seeds")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Make a synthetic confirmed readback in a private copy of the demo DB.

This never opens the user's database or contacts Samsung. Input is the exact
queued stroke captured from the isolated offline test engine.
"""
import importlib.util
import json
from pathlib import Path
import sqlite3
import sys

root = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(root / 'reference/python'))
from samsung_notes.document import DocumentSnapshot
from samsung_notes.ink_capture import prepare_ink_append_v3
from samsung_notes.ink_editor import StrokeSamples

spec = importlib.util.spec_from_file_location('demo_generator', root / 'reference/python/generators/demo/gen_demo_library.py')
demo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(demo)
data = json.load(sys.stdin)
database = Path(sys.argv[1]).resolve()
if not database.is_relative_to(Path('/tmp').resolve()):
    raise SystemExit('synthetic database must be under /tmp')
with sqlite3.connect(database) as db:
    row = db.execute('SELECT base_document, revision FROM notes WHERE id=?', (data['noteId'],)).fetchone()
    if row is None:
        raise SystemExit('synthetic note missing')
    snapshot = DocumentSnapshot.from_dict(json.loads(row[0]))
    stroke = data['stroke']
    points = stroke['points']
    samples = StrokeSamples(tuple((p['x'], p['y']) for p in points),
                            tuple(p['pressure'] for p in points),
                            tuple(p['timeMs'] for p in points))
    edit = prepare_ink_append_v3(snapshot, stroke['sourceStrokeId'], new_stroke_id=stroke['id'],
                                 samples=samples, now_ms=1793093400000,
                                 color_argb=stroke['colorARGB'], pen_size=stroke['size'])
    committed = demo.committed(edit, snapshot, 'synthetic-readback')
    db.execute('UPDATE notes SET base_document=?, revision=?, sync_state=? WHERE id=?',
               (json.dumps(committed.to_dict()), row[1] + 1, 'synced', data['noteId']))
    db.execute('DELETE FROM ink_documents WHERE note_id=?', (data['noteId'],))
print('synthetic readback prepared')

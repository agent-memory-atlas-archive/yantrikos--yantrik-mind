"""Read-only check of a Yantrik Mind database, for the #411 move: integrity plus a row count of every
table, as JSON. Run once before the move (as yantrik) and once after (as yantrik-mind), with the
service stopped both times, then compare the two outputs.

    python3 mind_db_check.py /path/to/mind.db > before.json
"""
import json, sqlite3, sys

path = sys.argv[1]
db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
tables = [r[0] for r in db.execute(
    "select name from sqlite_master where type='table' and name not like 'sqlite_%' order by name")]
counts = {}
for t in tables:
    try:
        counts[t] = db.execute(f'select count(*) from "{t}"').fetchone()[0]
    except sqlite3.DatabaseError as e:  # an FTS shadow table can refuse a plain count; say so
        counts[t] = f"error: {e}"
print(json.dumps({
    "path": path,
    "integrity": db.execute("pragma integrity_check").fetchone()[0],
    "journal_mode": db.execute("pragma journal_mode").fetchone()[0],
    "tables": len(tables),
    "counts": counts,
}, indent=1, sort_keys=True))

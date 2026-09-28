"""Compare two mind_db_check.py outputs: integrity ok on both, and every table's count identical."""
import json, sys

a, b = (json.load(open(p)) for p in sys.argv[1:3])
problems = []
for side, d in (("before", a), ("after", b)):
    if d["integrity"] != "ok":
        problems.append(f"{side}: integrity_check = {d['integrity']}")
for t in sorted(set(a["counts"]) | set(b["counts"])):
    x, y = a["counts"].get(t, "missing"), b["counts"].get(t, "missing")
    if x != y:
        problems.append(f"{t}: {x} -> {y}")
print("MATCH" if not problems else "MISMATCH")
for p in problems:
    print(" ", p)
print(f"{len(a['counts'])} tables compared; memories {a['counts'].get('memories')} -> {b['counts'].get('memories')}, "
      f"conversation_turns {a['counts'].get('conversation_turns')} -> {b['counts'].get('conversation_turns')}")
sys.exit(1 if problems else 0)

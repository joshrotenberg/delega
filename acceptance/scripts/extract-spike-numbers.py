#!/usr/bin/env python3
"""Copies the numbers COMPARISON.md needs out of the outcome spike's results.

Usage: python3 -I extract-spike-numbers.py SPIKES_DIR > spike-numbers.jsonl

SPIKES_DIR is the `spikes/` directory of the spike worktree
(delega-dot-spike-outcomes, commit 7f4da23). It is read, never changed.

One line per spike row that has a counterpart in this harness's matrix
(provider, case and variant): the side (`counterpart`, `due-oneshot`,
`due-resident`), the status the side reported, the time to the result in
milliseconds, whether the status was a timeout, and the survivors measured 5 s
after the result (after close and shutdown for the resident sides), as the
spike's make-comparison.py reads them. Rows run only by the spike's `j`, `j2`,
`f`, `g` and `h` cases are not copied.
"""
import json
import sys
from pathlib import Path

KEEP = {"a", "b", "c", "d", "e", "ed", "fd", "hd", "i", "id"}


def load(path):
    return [json.loads(line) for line in open(path)]


def line(side, row):
    stages = row["stages"]
    first = stages[0]
    final = stages[1] if side != "due-oneshot" and len(stages) > 1 else first
    star = side != "due-oneshot" and len(stages) > 1 and first["survivors_5s"] != "none" and final["survivors_5s"] == "none"
    return {
        "id": f'{row["provider"]}-{row["case"]}-{row["variant"]}',
        "side": side,
        "status": first.get("status"),
        "ms": first.get("elapsed_ms"),
        "timeout": first.get("message") == "Turn exceeded timeout",
        "survivors": final.get("survivors_5s", "?"),
        "only_after_close": star,
    }


def main():
    spikes = Path(sys.argv[1])
    out = []
    for row in load(spikes / "counterpart-outcomes" / "results.jsonl"):
        if row["case"] in KEEP:
            out.append(line("counterpart", row))
    for row in load(spikes / "due-outcomes" / "results.jsonl"):
        if row["case"] not in KEEP or row.get("stream") or row["variant"].startswith("orphan"):
            continue
        if row["path"] == "oneshot":
            out.append(line("due-oneshot", row))
        elif row["path"] == "resident":
            out.append(line("due-resident", row))
    out.sort(key=lambda r: (r["id"], r["side"]))
    for r in out:
        print(json.dumps(r, sort_keys=True))


main()

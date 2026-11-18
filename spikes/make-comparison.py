#!/usr/bin/env python3
"""Builds the side-by-side table in COMPARISON.md from the two results files.

Usage: python3 spikes/make-comparison.py > table.md

Cell rule (the same for every side):
  C1 ok    the caller received a stated outcome (completed, failed, timed out,
           interrupted). Otherwise the cell names the gap: HANG (no result at
           the harness limit), Err (an error instead of a result), none (the
           future or handle was dropped, so no result exists by construction).
  C2 ok    no fake-provider process was alive 5 s after the call returned (due
           one-shot), or 5 s after the worker was closed and the engine or
           supervisor shut down (due resident, counterpart). Otherwise the cell
           lists the survivors. `ok*` marks a row where a process was still alive
           5 s after the result and only the close or shutdown removed it (the
           resident worker, or a run that never settled).
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
STATED = {"Succeeded", "Failed", "TimedOut", "Cancelled", "completed", "failed", "interrupted"}


def load(path):
    return {json.loads(l)["id"]: json.loads(l) for l in open(path)}


due = load(HERE / "due-outcomes" / "results.jsonl")
cp = load(HERE / "counterpart-outcomes" / "results.jsonl")


def c1(stage, case):
    status = stage.get("status")
    if status in STATED:
        return "ok", status
    if case in ("j", "j2"):
        return "none", "no result"
    if status == "HANG":
        return "HANG", "HANG"
    return "Err", str(status)


def cell(row, kind):
    if row is None:
        return "not run"
    st = row["stages"][0]
    case = row["case"]
    ok1, label = c1(st, case)
    if kind == "cp" and ok1 == "ok" and st.get("message") == "Turn exceeded timeout":
        label = "failed (timeout)"
    ms = st.get("elapsed_ms")
    final = st
    star = ""
    if kind in ("resident", "cp") and len(row["stages"]) > 1:
        final = row["stages"][1]
        if st["survivors_5s"] != "none" and final["survivors_5s"] == "none":
            star = "*"
    surv = final.get("survivors_5s", "?")
    c2 = "ok" + star if surv == "none" else surv
    left = c1_text(ok1, label)
    return f"{left} {ms} ms; C2 {c2}"


def c1_text(ok1, label):
    return f"C1 {label}" if ok1 == "ok" else f"C1 GAP {label}"


rows = []
for rid, r in sorted(cp.items(), key=lambda kv: (kv[1]["provider"], kv[1]["case"], kv[1]["variant"])):
    prov, case, var = r["provider"], r["case"], r["variant"]
    d1 = due.get(f"oneshot-{prov}-{case}-{var}")
    d2 = due.get(f"resident-{prov}-{case}-{var}")
    if var.startswith("orphan"):
        d1txt = d2txt = "not run"
    else:
        d1txt = cell(d1, "oneshot") if d1 else "n/a"
        d2txt = cell(d2, "resident") if d2 else "n/a"
    rows.append((prov, case, var, d1txt, d2txt, cell(r, "cp")))

print("| Provider | Case | Variant | due one-shot | due resident | counterpart |")
print("| --- | --- | --- | --- | --- | --- |")
for row in rows:
    print("| " + " | ".join(x.replace("|", "\\|") for x in row) + " |")

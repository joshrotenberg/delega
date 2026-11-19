# Results: does delega meet criteria 1 and 2?

Generated from `results.jsonl` by `cargo run -p delega-acceptance -- --report`; do not edit by hand.
The harness is described in [README.md](README.md). Criteria, from the repository README:
(1) every run ends in exactly one stated outcome, never a hang and never silence; (2) after any
outcome the provider and its descendants are gone, or the result says cleanup could not be
confirmed and what remains.

## Environment

- Machine: Darwin 27.2.0 arm64.
- delega: version 0.1.0-dev.
- Harness commit: `dfcf10e`.
- 152 rows, 8 at a time, finished in 74 s.
- Fake providers only, a temporary `HOME` and a cleared environment per case; no real `claude` or `codex`.
- Survivors are measured by a per-case marker in the process table and by a heartbeat file, 1 s after
`delega` exits, whatever `delega` reported; the harness then kills them.
- Cases run in parallel, so differences under about 100 ms mean nothing.

## Summary

- **Expected outcome and exit**: the outcome and the exit status the expectations table gives.
- **One stated outcome**: standard output held exactly one JSON document that parses and validates
as a `RunResult`, and `delega` exited without the harness having to kill it. Rows where the harness
closed standard output or killed `delega` have no document to read and are not counted.
- **Nothing left**: no process carrying the case marker was alive 1 s after `delega` exited.
- **Honest report**: survivors exist only when `cleanup.confirmed` is false. Counted over rows that
have a result.

| Group | Rows | Expected outcome and exit | One stated outcome | Nothing left | Honest report | Pass | Expected failure | Unexpected pass | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| all | 152 | 152 | 136 of 136 | 116 | 108 of 136 | 116 | 36 | 0 | 0 |
| claude | 76 | 76 | 68 of 68 | 58 | 54 of 68 | 58 | 18 | 0 | 0 |
| codex | 76 | 76 | 68 of 68 | 58 | 54 of 68 | 58 | 18 | 0 | 0 |

### By caller action

| Group | Rows | Expected outcome and exit | One stated outcome | Nothing left | Honest report | Pass | Expected failure | Unexpected pass | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| provider ends by itself | 52 | 52 | 52 of 52 | 36 | 36 of 52 | 36 | 16 | 0 | 0 |
| time limit (--timeout) | 46 | 46 | 46 of 46 | 38 | 38 of 46 | 38 | 8 | 0 | 0 |
| SIGINT | 22 | 22 | 22 of 22 | 18 | 18 of 22 | 18 | 4 | 0 | 0 |
| SIGTERM | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 0 | 0 | 0 |
| SIGHUP | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 0 | 0 | 0 |
| stdout closed early | 8 | 8 | 0 of 0 | 8 | 0 of 0 | 8 | 0 | 0 | 0 |
| SIGKILL of delega | 8 | 8 | 0 of 0 | 0 | 0 of 0 | 0 | 8 | 0 | 0 |

### By variant (what the provider leaves running)

| Group | Rows | Expected outcome and exit | One stated outcome | Nothing left | Honest report | Pass | Expected failure | Unexpected pass | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| plain | 34 | 34 | 28 of 28 | 32 | 28 of 28 | 32 | 2 | 0 | 0 |
| child | 28 | 28 | 24 of 24 | 26 | 24 of 24 | 26 | 2 | 0 | 0 |
| pipe | 12 | 12 | 12 of 12 | 12 | 12 of 12 | 12 | 0 | 0 | 0 |
| ignore | 12 | 12 | 12 of 12 | 12 | 12 of 12 | 12 | 0 | 0 | 0 |
| stubborn | 14 | 14 | 12 of 12 | 12 | 12 of 12 | 12 | 2 | 0 | 0 |
| setsid | 22 | 22 | 18 of 18 | 14 | 12 of 18 | 14 | 8 | 0 | 0 |
| orphan | 10 | 10 | 10 of 10 | 0 | 0 of 10 | 0 | 10 | 0 | 0 |
| orphan-late | 10 | 10 | 10 of 10 | 8 | 8 of 10 | 8 | 2 | 0 | 0 |
| relay | 10 | 10 | 10 of 10 | 0 | 0 of 10 | 0 | 10 | 0 | 0 |

## Latency

Measured from the harness's action to `delega` exiting (a signal), past the `--timeout` (a time
limit that fired), or from start to exit (a run left to finish). A process that ignores TERM adds the
grace period before SIGKILL. SIGKILL rows are left out.

| Measure | Rows | p50 | p90 | p99 | max |
| --- | --- | --- | --- | --- | --- |
| signal to exit | 30 | 188 ms | 211 ms | 275 ms | 275 ms |
| signal to exit, a process ignores TERM | 8 | 1692 ms | 1761 ms | 1761 ms | 1761 ms |
| timeout overshoot | 42 | 194 ms | 235 ms | 269 ms | 269 ms |
| timeout overshoot, a process ignores TERM | 10 | 1699 ms | 1747 ms | 1772 ms | 1772 ms |
| run to exit | 48 | 613 ms | 1039 ms | 1980 ms | 1980 ms |
| run to exit, a process ignores TERM | 6 | 2069 ms | 2472 ms | 2472 ms | 2472 ms |

## Failures that are not expected

None. Every row either met its expectation or is a listed expected failure.

## Expected failures

Rows in the expectations table (`acceptance/src/matrix.rs`) that are expected to fail criterion 2.
A *documented gap* is one the process module lists as known; a *defect* is a failure this harness
found that is not documented. A row in this list that passes fails the run, so the table is
corrected when a gap closes.

### documented gap: 10 rows, 10 failed as expected in this run

A descendant that called setsid and was orphaned before any scan saw its parent is in no group or session of the worker and has no known ancestry, so cleanup reports confirmed while it runs (process module gap 1, delega issue 1).

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest |
| --- | --- | --- | --- | --- | --- |
| claude-a-orphan | completed | 0 | yes | leaf-setsid | NO |
| claude-c-orphan | timed_out | 124 | yes | leaf-setsid | NO |
| claude-d-orphan | interrupted | 130 | yes | leaf-setsid | NO |
| claude-e-orphan | failed | 1 | yes | leaf-setsid | NO |
| claude-fd-orphan | timed_out | 124 | yes | leaf-setsid | NO |
| codex-a-orphan | completed | 0 | yes | leaf-setsid | NO |
| codex-c-orphan | timed_out | 124 | yes | leaf-setsid | NO |
| codex-d-orphan | interrupted | 130 | yes | leaf-setsid | NO |
| codex-e-orphan | failed | 1 | yes | leaf-setsid | NO |
| codex-fd-orphan | timed_out | 124 | yes | leaf-setsid | NO |

### documented gap: 10 rows, 10 failed as expected in this run

A fast fork relay is never in a process listing for long enough to be a member, so cleanup reports confirmed while it runs (process module gap 2, delega issue 1).

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest |
| --- | --- | --- | --- | --- | --- |
| claude-a-relay | completed | 0 | yes | relay-heartbeat | NO |
| claude-c-relay | timed_out | 124 | yes | relay-heartbeat | NO |
| claude-d-relay | interrupted | 130 | yes | relay-heartbeat | NO |
| claude-e-relay | failed | 1 | yes | relay-heartbeat | NO |
| claude-fd-relay | timed_out | 124 | yes | relay-heartbeat | NO |
| codex-a-relay | completed | 0 | yes | relay-heartbeat | NO |
| codex-c-relay | timed_out | 124 | yes | relay-heartbeat | NO |
| codex-d-relay | interrupted | 130 | yes | relay-heartbeat | NO |
| codex-e-relay | failed | 1 | yes | relay-heartbeat | NO |
| codex-fd-relay | timed_out | 124 | yes | relay-heartbeat | NO |

### documented gap: 8 rows, 8 failed as expected in this run

SIGKILL of delega cannot be handled, so the worker keeps running with nothing enforcing its time limit (docs/api.md, process module docs). Documented gap.

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest |
| --- | --- | --- | --- | --- | --- |
| claude-k-plain | none | signal 9 | n/a | provider | n/a |
| claude-k-child | none | signal 9 | n/a | child,provider | n/a |
| claude-k-setsid | none | signal 9 | n/a | leaf-setsid,mid,provider | n/a |
| claude-k-stubborn | none | signal 9 | n/a | provider | n/a |
| codex-k-plain | none | signal 9 | n/a | provider | n/a |
| codex-k-child | none | signal 9 | n/a | child,provider | n/a |
| codex-k-setsid | none | signal 9 | n/a | leaf-setsid,mid,provider | n/a |
| codex-k-stubborn | none | signal 9 | n/a | provider | n/a |

### documented gap: 8 rows, 8 failed as expected in this run

The provider ended on its own (completed, failed, or output without a final answer) before any 500 ms scan saw its setsid descendant. No stop starts, so there is no scan before the final sweep, and the descendant is in no group or session of the worker. Same mechanism as process module gap 1, reached by a normal end, which the module docs list only for an orphaned parent and a crash.

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest |
| --- | --- | --- | --- | --- | --- |
| claude-a-setsid | completed | 0 | yes | leaf-setsid | NO |
| claude-a-orphan-late | completed | 0 | yes | leaf-setsid | NO |
| claude-b-setsid | failed | 1 | yes | leaf-setsid | NO |
| claude-i-setsid | failed | 1 | yes | leaf-setsid | NO |
| codex-a-setsid | completed | 0 | yes | leaf-setsid | NO |
| codex-a-orphan-late | completed | 0 | yes | leaf-setsid | NO |
| codex-b-setsid | failed | 1 | yes | leaf-setsid | NO |
| codex-i-setsid | failed | 1 | yes | leaf-setsid | NO |

## Variation across repeated runs

`repeats.jsonl` holds 3 full runs of the whole matrix (152 rows each).

| Run | Pass | Expected failure | Unexpected pass | Fail | p99 latency (not SIGKILL) | Max latency |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 116 | 36 | 0 | 0 | 2402 ms | 2601 ms |
| 2 | 116 | 36 | 0 | 0 | 2427 ms | 2594 ms |
| 3 | 116 | 36 | 0 | 0 | 2412 ms | 2528 ms |

No row changed verdict, outcome, exit status, survivors or cleanup report between runs.

## All rows

| Row | Action | Outcome | Exit | One document | Cleanup confirmed | Left running | Honest | Latency | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| claude-a-plain | wait | completed | 0 | yes | yes | none | yes | 563 ms | pass |
| claude-a-child | wait | completed | 0 | yes | yes | none | yes | 655 ms | pass |
| claude-a-setsid | wait | completed | 0 | yes | yes | leaf-setsid | NO | 651 ms | expected_failure |
| claude-a-ignore | wait | completed | 0 | yes | yes | none | yes | 2169 ms | pass |
| claude-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 565 ms | pass |
| claude-a-pipe | wait | completed | 0 | yes | yes | none | yes | 599 ms | pass |
| claude-a-orphan | wait | completed | 0 | yes | yes | leaf-setsid | NO | 679 ms | expected_failure |
| claude-a-orphan-late | wait | completed | 0 | yes | yes | leaf-setsid | NO | 629 ms | expected_failure |
| claude-a-relay | wait | completed | 0 | yes | yes | relay-heartbeat | NO | 425 ms | expected_failure |
| claude-b-plain | wait | failed | 1 | yes | yes | none | yes | 328 ms | pass |
| claude-b-child | wait | failed | 1 | yes | yes | none | yes | 397 ms | pass |
| claude-b-setsid | wait | failed | 1 | yes | yes | leaf-setsid | NO | 387 ms | expected_failure |
| claude-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 197 ms | pass |
| claude-c-child | wait | timed_out | 124 | yes | yes | none | yes | 197 ms | pass |
| claude-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 226 ms | pass |
| claude-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1641 ms | pass |
| claude-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1716 ms | pass |
| claude-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 237 ms | pass |
| claude-c-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 200 ms | expected_failure |
| claude-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 221 ms | pass |
| claude-c-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 183 ms | expected_failure |
| claude-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 172 ms | pass |
| claude-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 167 ms | pass |
| claude-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 203 ms | pass |
| claude-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1654 ms | pass |
| claude-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1692 ms | pass |
| claude-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 198 ms | pass |
| claude-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | leaf-setsid | NO | 178 ms | expected_failure |
| claude-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 206 ms | pass |
| claude-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | relay-heartbeat | NO | 163 ms | expected_failure |
| claude-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 188 ms | pass |
| claude-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 199 ms | pass |
| claude-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 194 ms | pass |
| claude-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1727 ms | pass |
| claude-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 162 ms | pass |
| claude-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 161 ms | pass |
| claude-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 188 ms | pass |
| claude-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1743 ms | pass |
| claude-e-plain | wait | failed | 1 | yes | yes | none | yes | 814 ms | pass |
| claude-e-child | wait | failed | 1 | yes | yes | none | yes | 855 ms | pass |
| claude-e-setsid | wait | failed | 1 | yes | yes | none | yes | 867 ms | pass |
| claude-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2360 ms | pass |
| claude-e-pipe | wait | failed | 1 | yes | yes | none | yes | 902 ms | pass |
| claude-e-orphan | wait | failed | 1 | yes | yes | leaf-setsid | NO | 911 ms | expected_failure |
| claude-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 900 ms | pass |
| claude-e-relay | wait | failed | 1 | yes | yes | relay-heartbeat | NO | 896 ms | expected_failure |
| claude-ed-plain | wait | failed | 1 | yes | yes | none | yes | 806 ms | pass |
| claude-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 893 ms | pass |
| claude-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 160 ms | pass |
| claude-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 181 ms | pass |
| claude-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 193 ms | pass |
| claude-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1633 ms | pass |
| claude-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1699 ms | pass |
| claude-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 184 ms | pass |
| claude-fd-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 182 ms | expected_failure |
| claude-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 196 ms | pass |
| claude-fd-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 189 ms | expected_failure |
| claude-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 187 ms | pass |
| claude-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 205 ms | pass |
| claude-i-plain | wait | failed | 1 | yes | yes | none | yes | 264 ms | pass |
| claude-i-child | wait | failed | 1 | yes | yes | none | yes | 312 ms | pass |
| claude-i-setsid | wait | failed | 1 | yes | yes | leaf-setsid | NO | 352 ms | expected_failure |
| claude-id-plain | wait | failed | 1 | yes | yes | none | yes | 266 ms | pass |
| claude-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 251 ms | pass |
| claude-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 267 ms | pass |
| claude-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1772 ms | pass |
| claude-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 210 ms | pass |
| claude-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 275 ms | pass |
| claude-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 215 ms | pass |
| claude-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 178 ms | pass |
| claude-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 199 ms | pass |
| claude-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1804 ms | pass |
| claude-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 7 ms | expected_failure |
| claude-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | child,provider | n/a | 6 ms | expected_failure |
| claude-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | leaf-setsid,mid,provider | n/a | 7 ms | expected_failure |
| claude-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 8 ms | expected_failure |
| codex-a-plain | wait | completed | 0 | yes | yes | none | yes | 438 ms | pass |
| codex-a-child | wait | completed | 0 | yes | yes | none | yes | 540 ms | pass |
| codex-a-setsid | wait | completed | 0 | yes | yes | leaf-setsid | NO | 613 ms | expected_failure |
| codex-a-ignore | wait | completed | 0 | yes | yes | none | yes | 2069 ms | pass |
| codex-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 450 ms | pass |
| codex-a-pipe | wait | completed | 0 | yes | yes | none | yes | 457 ms | pass |
| codex-a-orphan | wait | completed | 0 | yes | yes | leaf-setsid | NO | 406 ms | expected_failure |
| codex-a-orphan-late | wait | completed | 0 | yes | yes | leaf-setsid | NO | 429 ms | expected_failure |
| codex-a-relay | wait | completed | 0 | yes | yes | relay-heartbeat | NO | 501 ms | expected_failure |
| codex-b-plain | wait | failed | 1 | yes | yes | none | yes | 496 ms | pass |
| codex-b-child | wait | failed | 1 | yes | yes | none | yes | 499 ms | pass |
| codex-b-setsid | wait | failed | 1 | yes | yes | leaf-setsid | NO | 484 ms | expected_failure |
| codex-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 185 ms | pass |
| codex-c-child | wait | timed_out | 124 | yes | yes | none | yes | 194 ms | pass |
| codex-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 150 ms | pass |
| codex-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1655 ms | pass |
| codex-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1710 ms | pass |
| codex-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 196 ms | pass |
| codex-c-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 193 ms | expected_failure |
| codex-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 203 ms | pass |
| codex-c-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 235 ms | expected_failure |
| codex-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 164 ms | pass |
| codex-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 219 ms | pass |
| codex-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 191 ms | pass |
| codex-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1633 ms | pass |
| codex-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1708 ms | pass |
| codex-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 179 ms | pass |
| codex-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | leaf-setsid | NO | 176 ms | expected_failure |
| codex-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 207 ms | pass |
| codex-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | relay-heartbeat | NO | 211 ms | expected_failure |
| codex-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 195 ms | pass |
| codex-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 192 ms | pass |
| codex-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 203 ms | pass |
| codex-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1689 ms | pass |
| codex-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 185 ms | pass |
| codex-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 180 ms | pass |
| codex-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 154 ms | pass |
| codex-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1761 ms | pass |
| codex-e-plain | wait | failed | 1 | yes | yes | none | yes | 954 ms | pass |
| codex-e-child | wait | failed | 1 | yes | yes | none | yes | 1023 ms | pass |
| codex-e-setsid | wait | failed | 1 | yes | yes | none | yes | 1014 ms | pass |
| codex-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2472 ms | pass |
| codex-e-pipe | wait | failed | 1 | yes | yes | none | yes | 1060 ms | pass |
| codex-e-orphan | wait | failed | 1 | yes | yes | leaf-setsid | NO | 1039 ms | expected_failure |
| codex-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 1003 ms | pass |
| codex-e-relay | wait | failed | 1 | yes | yes | relay-heartbeat | NO | 1017 ms | expected_failure |
| codex-ed-plain | wait | failed | 1 | yes | yes | none | yes | 993 ms | pass |
| codex-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 1042 ms | pass |
| codex-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 180 ms | pass |
| codex-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 155 ms | pass |
| codex-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 172 ms | pass |
| codex-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1642 ms | pass |
| codex-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1700 ms | pass |
| codex-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 172 ms | pass |
| codex-fd-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 205 ms | expected_failure |
| codex-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 202 ms | pass |
| codex-fd-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 201 ms | expected_failure |
| codex-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 189 ms | pass |
| codex-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 177 ms | pass |
| codex-i-plain | wait | failed | 1 | yes | yes | none | yes | 371 ms | pass |
| codex-i-child | wait | failed | 1 | yes | yes | none | yes | 472 ms | pass |
| codex-i-setsid | wait | failed | 1 | yes | yes | leaf-setsid | NO | 475 ms | expected_failure |
| codex-id-plain | wait | failed | 1 | yes | yes | none | yes | 392 ms | pass |
| codex-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 220 ms | pass |
| codex-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 269 ms | pass |
| codex-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1747 ms | pass |
| codex-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 180 ms | pass |
| codex-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 239 ms | pass |
| codex-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 194 ms | pass |
| codex-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 176 ms | pass |
| codex-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 188 ms | pass |
| codex-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1980 ms | pass |
| codex-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 7 ms | expected_failure |
| codex-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | child,provider | n/a | 6 ms | expected_failure |
| codex-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | leaf-setsid,mid,provider | n/a | 7 ms | expected_failure |
| codex-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 7 ms | expected_failure |

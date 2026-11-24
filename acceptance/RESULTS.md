# Results: does delega meet criteria 1 and 2?

Generated from `results.jsonl` by `cargo run -p delega-acceptance -- --report`; do not edit by hand.
The harness is described in [README.md](README.md). Criteria, from the repository README:
(1) every run ends in exactly one stated outcome, never a hang and never silence; (2) after any
outcome the provider and its descendants are gone, or the result says cleanup could not be
confirmed and what remains.

## Environment

- Machine: Darwin 27.2.0 arm64.
- The system hides the environment of its own programs from `ps -E`: yes. On macOS the orphan rows are listed as gap cases when it does and ordinary cases when it does not (process module docs, "What remains possible").
- delega: version 0.1.0-dev.
- Harness commit: `7b6f4fe`.
- 202 rows, 8 at a time, finished in 93 s.
- Fake providers only, a temporary `HOME` and a cleared environment per case; no real `claude` or `codex`.
- Survivors are measured by a per-case marker in the process table and by a heartbeat file, 1 s after
`delega` exits, whatever `delega` reported; the harness then kills them.
- Cases run in parallel, so differences under about 100 ms mean nothing.

## Summary

- **Expected outcome and exit**: the outcome and the exit status the expectations table gives.
- **Cleanup verdict as expected**: `cleanup.confirmed` matches the fixture's reference value. The reference outcome, exit and cleanup verdict are recorded for comparison.
- **One stated outcome**: standard output held exactly one JSON document that parses and validates
as a `RunResult`, and `delega` exited without the harness having to kill it. Rows where the harness
closed standard output or killed `delega` have no document to read and are not counted.
- **Nothing left**: no process carrying the case marker was alive 1 s after `delega` exited.
- **Honest report**: a Confirmed result had no observed survivor at exit or after settling. Counted over rows that have a result.
- **Gap shown** and **Gap not shown**: observations of the listed survivor roles after settling, independent of the verdict.

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Honest report | Pass | Gap shown | Gap not shown | Skipped | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| all | 202 | 202 | 202 | 172 of 172 | 165 | 151 of 172 | 181 | 27 | 1 | 0 | 21 |
| claude | 101 | 101 | 101 | 86 of 86 | 83 | 76 of 86 | 91 | 13 | 1 | 0 | 10 |
| codex | 101 | 101 | 101 | 86 of 86 | 82 | 75 of 86 | 90 | 14 | 0 | 0 | 11 |

### By caller action

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Honest report | Pass | Gap shown | Gap not shown | Skipped | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| provider ends by itself | 56 | 56 | 56 | 56 of 56 | 47 | 47 of 56 | 47 | 9 | 1 | 0 | 9 |
| time limit (--timeout) | 50 | 50 | 50 | 50 of 50 | 42 | 42 of 50 | 42 | 8 | 0 | 0 | 8 |
| SIGINT | 24 | 24 | 24 | 24 of 24 | 20 | 20 of 24 | 20 | 4 | 0 | 0 | 4 |
| SIGTERM | 8 | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 0 | 0 | 0 | 0 |
| SIGHUP | 8 | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 0 | 0 | 0 | 0 |
| stdout closed early | 8 | 8 | 8 | 0 of 0 | 8 | 0 of 0 | 8 | 0 | 0 | 0 | 0 |
| SIGKILL of delega | 20 | 20 | 20 | 0 of 0 | 16 | 0 of 0 | 20 | 4 | 0 | 0 | 0 |
| SIGKILL of delega and its keeper | 2 | 2 | 2 | 0 of 0 | 0 | 0 of 0 | 2 | 2 | 0 | 0 | 0 |
| other | 26 | 26 | 26 | 26 of 26 | 16 | 26 of 26 | 26 | 0 | 0 | 0 | 0 |

### By variant (what the provider leaves running)

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Honest report | Pass | Gap shown | Gap not shown | Skipped | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| plain | 42 | 42 | 42 | 34 of 34 | 40 | 34 of 34 | 42 | 2 | 0 | 0 | 0 |
| child | 34 | 34 | 34 | 30 of 30 | 34 | 30 of 30 | 34 | 0 | 0 | 0 | 0 |
| pipe | 14 | 14 | 14 | 12 of 12 | 14 | 12 of 12 | 14 | 0 | 0 | 0 | 0 |
| ignore | 16 | 16 | 16 | 14 of 14 | 16 | 14 of 14 | 16 | 0 | 0 | 0 | 0 |
| stubborn | 16 | 16 | 16 | 14 of 14 | 16 | 14 of 14 | 16 | 0 | 0 | 0 | 0 |
| setsid | 28 | 28 | 28 | 24 of 24 | 22 | 24 of 24 | 28 | 0 | 0 | 0 | 0 |
| orphan | 14 | 14 | 14 | 12 of 12 | 0 | 2 of 12 | 4 | 12 | 0 | 0 | 10 |
| orphan-late | 12 | 12 | 12 | 10 of 10 | 12 | 10 of 10 | 12 | 0 | 0 | 0 | 0 |
| relay | 12 | 12 | 12 | 10 of 10 | 0 | 0 of 10 | 2 | 12 | 0 | 0 | 10 |

## Latency

Measured from the harness's action to `delega` exiting (a signal), past the `--timeout` (a time
limit that fired), or from start to exit (a run left to finish). A process that ignores TERM adds the
grace period before SIGKILL. SIGKILL rows are left out.

| Measure | Rows | p50 | p90 | p99 | max |
| --- | --- | --- | --- | --- | --- |
| signal to exit | 40 | 166 ms | 238 ms | 256 ms | 256 ms |
| signal to exit, a process ignores TERM | 10 | 1696 ms | 1717 ms | 1757 ms | 1757 ms |
| timeout overshoot | 52 | 163 ms | 282 ms | 474 ms | 474 ms |
| timeout overshoot, a process ignores TERM | 12 | 1698 ms | 1796 ms | 1882 ms | 1882 ms |
| run to exit | 62 | 730 ms | 1279 ms | 3579 ms | 3579 ms |
| run to exit, a process ignores TERM | 6 | 2002 ms | 2552 ms | 2552 ms | 2552 ms |

## Failures and skips

| Row | Verdict | Problems |
| --- | --- | --- |
| claude-a-orphan | fail | cleanup was Confirmed with survivors (at exit: ["leaf-setsid"], after settling: ["leaf-setsid"]) |
| claude-a-relay | fail | cleanup was Confirmed with survivors (at exit: ["relay-heartbeat"], after settling: ["relay-heartbeat"]) |
| claude-c-orphan | fail | cleanup was Confirmed with survivors (at exit: ["leaf-setsid"], after settling: ["leaf-setsid"]) |
| claude-c-relay | fail | cleanup was Confirmed with survivors (at exit: ["relay-heartbeat"], after settling: ["relay-heartbeat"]) |
| claude-d-orphan | fail | cleanup was Confirmed with survivors (at exit: ["leaf-setsid"], after settling: ["leaf-setsid"]) |
| claude-d-relay | fail | cleanup was Confirmed with survivors (at exit: ["relay-heartbeat"], after settling: ["relay-heartbeat"]) |
| claude-e-orphan | fail | cleanup was Confirmed with survivors (at exit: ["leaf-setsid"], after settling: ["leaf-setsid"]) |
| claude-e-relay | fail | cleanup was Confirmed with survivors (at exit: ["relay-heartbeat"], after settling: ["relay-heartbeat"]) |
| claude-fd-orphan | fail | cleanup was Confirmed with survivors (at exit: ["leaf-setsid"], after settling: ["leaf-setsid"]) |
| claude-fd-relay | fail | cleanup was Confirmed with survivors (at exit: ["relay-heartbeat"], after settling: ["relay-heartbeat"]) |
| codex-a-orphan | fail | cleanup was Confirmed with survivors (at exit: ["leaf-setsid"], after settling: ["leaf-setsid"]) |
| codex-a-relay | fail | cleanup was Confirmed with survivors (at exit: ["relay-heartbeat"], after settling: ["relay-heartbeat"]) |
| codex-a-bare | fail | cleanup was Confirmed with survivors (at exit: ["bare"], after settling: ["bare"]) |
| codex-c-orphan | fail | cleanup was Confirmed with survivors (at exit: ["leaf-setsid"], after settling: ["leaf-setsid"]) |
| codex-c-relay | fail | cleanup was Confirmed with survivors (at exit: ["relay-heartbeat"], after settling: ["relay-heartbeat"]) |
| codex-d-orphan | fail | cleanup was Confirmed with survivors (at exit: ["leaf-setsid"], after settling: ["leaf-setsid"]) |
| codex-d-relay | fail | cleanup was Confirmed with survivors (at exit: ["relay-heartbeat"], after settling: ["relay-heartbeat"]) |
| codex-e-orphan | fail | cleanup was Confirmed with survivors (at exit: ["leaf-setsid"], after settling: ["leaf-setsid"]) |
| codex-e-relay | fail | cleanup was Confirmed with survivors (at exit: ["relay-heartbeat"], after settling: ["relay-heartbeat"]) |
| codex-fd-orphan | fail | cleanup was Confirmed with survivors (at exit: ["leaf-setsid"], after settling: ["leaf-setsid"]) |
| codex-fd-relay | fail | cleanup was Confirmed with survivors (at exit: ["relay-heartbeat"], after settling: ["relay-heartbeat"]) |

## Gap observations

The matrix lists survivor roles that identify each gap. A gap is shown when all its listed roles are observed after settling. Each observation is independent of the guarantee checks and has no effect on the verdict.

- documented gap: 28 rows.
- undocumented gap: 0 rows.
- defect: 0 rows.

### documented gap: 2 rows, 2 shown in this run

SIGKILL of delega and of its keeper together: no code of either runs, so nothing stops the worker or enforces its time limit. The accepted residual on every platform (process module docs, "What remains possible").

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest | Gap observation | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- |
| claude-kk-plain | none | signal 9 | n/a | provider | n/a | gap shown | pass |
| codex-kk-plain | none | signal 9 | n/a | provider | n/a | gap shown | pass |

### documented gap: 14 rows, 13 shown in this run

macOS residual on a system that hides the environment of its own programs from `ps -E` (System Integrity Protection does): the process left the provider's session and its parent is gone, so no listing ties it to the run, macOS has no subreaper, and the run marker is not visible because these fakes run sh, sleep and perl. Where the environment is visible the marker can find it. Linux closes this through the keeper (process module docs, "What remains possible").

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest | Gap observation | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- |
| claude-a-orphan | completed | 0 | yes | leaf-setsid | NO | gap shown | fail |
| claude-a-bare | completed | 0 | yes | none | yes | gap not shown | pass |
| claude-c-orphan | timed_out | 124 | yes | leaf-setsid | NO | gap shown | fail |
| claude-d-orphan | interrupted | 130 | yes | leaf-setsid | NO | gap shown | fail |
| claude-e-orphan | failed | 1 | yes | leaf-setsid | NO | gap shown | fail |
| claude-fd-orphan | timed_out | 124 | yes | leaf-setsid | NO | gap shown | fail |
| claude-k-orphan | none | signal 9 | n/a | leaf-setsid | n/a | gap shown | pass |
| codex-a-orphan | completed | 0 | yes | leaf-setsid | NO | gap shown | fail |
| codex-a-bare | completed | 0 | yes | bare | NO | gap shown | fail |
| codex-c-orphan | timed_out | 124 | yes | leaf-setsid | NO | gap shown | fail |
| codex-d-orphan | interrupted | 130 | yes | leaf-setsid | NO | gap shown | fail |
| codex-e-orphan | failed | 1 | yes | leaf-setsid | NO | gap shown | fail |
| codex-fd-orphan | timed_out | 124 | yes | leaf-setsid | NO | gap shown | fail |
| codex-k-orphan | none | signal 9 | n/a | leaf-setsid | n/a | gap shown | pass |

### documented gap: 12 rows, 12 shown in this run

macOS residual: a fast fork relay can leave each listing before cleanup signals it. Linux closes this through the keeper's own walk of its tree (process module docs, "What remains possible").

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest | Gap observation | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- |
| claude-a-relay | completed | 0 | yes | relay-heartbeat | NO | gap shown | fail |
| claude-c-relay | timed_out | 124 | yes | relay-heartbeat | NO | gap shown | fail |
| claude-d-relay | interrupted | 130 | yes | relay-heartbeat | NO | gap shown | fail |
| claude-e-relay | failed | 1 | yes | relay-heartbeat | NO | gap shown | fail |
| claude-fd-relay | timed_out | 124 | yes | relay-heartbeat | NO | gap shown | fail |
| claude-k-relay | none | signal 9 | n/a | relay-heartbeat | n/a | gap shown | pass |
| codex-a-relay | completed | 0 | yes | relay-heartbeat | NO | gap shown | fail |
| codex-c-relay | timed_out | 124 | yes | relay-heartbeat | NO | gap shown | fail |
| codex-d-relay | interrupted | 130 | yes | relay-heartbeat | NO | gap shown | fail |
| codex-e-relay | failed | 1 | yes | relay-heartbeat | NO | gap shown | fail |
| codex-fd-relay | timed_out | 124 | yes | relay-heartbeat | NO | gap shown | fail |
| codex-k-relay | none | signal 9 | n/a | relay-heartbeat | n/a | gap shown | pass |

## Rows with process listing disabled

These rows disable process listing through the test seam. The table records any cleanup reason and the processes the harness observed.

| Row | Outcome | Confirmed | Process groups | Pids | Left running | Reason |
| --- | --- | --- | --- | --- | --- | --- |
| claude-u-plain | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-u-child | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-u-setsid | completed | NO | [] | [] | leaf-setsid | process inspection was unavailable (ps could not be started (no ps pro |
| claude-u-orphan | completed | NO | [] | [] | leaf-setsid | process inspection was unavailable (ps could not be started (no ps pro |
| claude-u-bare | completed | NO | [] | [] | bare | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ud-plain | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ud-child | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ud-setsid | timed_out | NO | [] | [] | leaf-setsid | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ud-stubborn | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ui-plain | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ui-child | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ui-setsid | interrupted | NO | [] | [] | leaf-setsid | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ui-ignore | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-u-plain | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-u-child | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-u-setsid | completed | NO | [] | [] | leaf-setsid | process inspection was unavailable (ps could not be started (no ps pro |
| codex-u-orphan | completed | NO | [] | [] | leaf-setsid | process inspection was unavailable (ps could not be started (no ps pro |
| codex-u-bare | completed | NO | [] | [] | bare | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ud-plain | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ud-child | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ud-setsid | timed_out | NO | [] | [] | leaf-setsid | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ud-stubborn | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ui-plain | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ui-child | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ui-setsid | interrupted | NO | [] | [] | leaf-setsid | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ui-ignore | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |

## Where the time goes in a run

Measured by the reviewer on a release build, not by this harness. On an idle machine a plain Claude
run takes about 250 to 310 ms and a plain Codex run about 385 to 440 ms. Roughly 80 percent of that
is process listings: 8 for Claude and 12 for Codex, 24 to 33 ms each, four per child process run
(the version probe counts as one, and Codex runs a second probe). Under 8 parallel jobs the listings
slow down, which accounts for the 264 to 680 ms seen for a normal completion here.

## Variation across repeated runs

`repeats.jsonl` holds 3 full runs of the whole matrix (202 rows each).

| Run | Pass | Gap shown | Gap not shown | Skipped | Fail | p99 latency (not SIGKILL) | Max latency |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 180 | 28 | 0 | 0 | 22 | 2681 ms | 3249 ms |
| 2 | 180 | 28 | 0 | 0 | 22 | 3262 ms | 3449 ms |
| 3 | 180 | 28 | 0 | 0 | 22 | 2754 ms | 2919 ms |

No row changed verdict, outcome, exit status, survivors or cleanup report between runs.

## All rows

| Row | Action | Outcome | Exit | One document | Cleanup confirmed | Left running | Honest | Latency | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| claude-a-plain | wait | completed | 0 | yes | yes | none | yes | 610 ms | pass |
| claude-a-child | wait | completed | 0 | yes | yes | none | yes | 724 ms | pass |
| claude-a-setsid | wait | completed | 0 | yes | yes | none | yes | 730 ms | pass |
| claude-a-ignore | wait | completed | 0 | yes | yes | none | yes | 2252 ms | pass |
| claude-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 621 ms | pass |
| claude-a-pipe | wait | completed | 0 | yes | yes | none | yes | 730 ms | pass |
| claude-a-orphan | wait | completed | 0 | yes | yes | leaf-setsid | NO | 1030 ms | fail |
| claude-a-orphan-late | wait | completed | 0 | yes | yes | none | yes | 741 ms | pass |
| claude-a-relay | wait | completed | 0 | yes | yes | relay-heartbeat | NO | 361 ms | fail |
| claude-a-bare | wait | completed | 0 | yes | yes | none | yes | 3579 ms | pass |
| claude-b-plain | wait | failed | 1 | yes | yes | none | yes | 253 ms | pass |
| claude-b-child | wait | failed | 1 | yes | yes | none | yes | 375 ms | pass |
| claude-b-setsid | wait | failed | 1 | yes | yes | none | yes | 375 ms | pass |
| claude-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 180 ms | pass |
| claude-c-child | wait | timed_out | 124 | yes | yes | none | yes | 157 ms | pass |
| claude-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 155 ms | pass |
| claude-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1691 ms | pass |
| claude-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1714 ms | pass |
| claude-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 159 ms | pass |
| claude-c-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 156 ms | fail |
| claude-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 163 ms | pass |
| claude-c-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 163 ms | fail |
| claude-c-bare | wait | timed_out | 124 | yes | yes | none | yes | 163 ms | pass |
| claude-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 151 ms | pass |
| claude-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 152 ms | pass |
| claude-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 164 ms | pass |
| claude-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1658 ms | pass |
| claude-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1701 ms | pass |
| claude-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 145 ms | pass |
| claude-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | leaf-setsid | NO | 171 ms | fail |
| claude-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 163 ms | pass |
| claude-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | relay-heartbeat | NO | 143 ms | fail |
| claude-d-bare | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 169 ms | pass |
| claude-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 165 ms | pass |
| claude-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 167 ms | pass |
| claude-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 145 ms | pass |
| claude-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1696 ms | pass |
| claude-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 153 ms | pass |
| claude-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 174 ms | pass |
| claude-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 171 ms | pass |
| claude-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1715 ms | pass |
| claude-e-plain | wait | failed | 1 | yes | yes | none | yes | 794 ms | pass |
| claude-e-child | wait | failed | 1 | yes | yes | none | yes | 844 ms | pass |
| claude-e-setsid | wait | failed | 1 | yes | yes | none | yes | 843 ms | pass |
| claude-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2364 ms | pass |
| claude-e-pipe | wait | failed | 1 | yes | yes | none | yes | 851 ms | pass |
| claude-e-orphan | wait | failed | 1 | yes | yes | leaf-setsid | NO | 1169 ms | fail |
| claude-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 855 ms | pass |
| claude-e-relay | wait | failed | 1 | yes | yes | relay-heartbeat | NO | 875 ms | fail |
| claude-e-bare | wait | failed | 1 | yes | yes | none | yes | 1875 ms | pass |
| claude-ed-plain | wait | failed | 1 | yes | yes | none | yes | 802 ms | pass |
| claude-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 853 ms | pass |
| claude-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 151 ms | pass |
| claude-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 163 ms | pass |
| claude-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 172 ms | pass |
| claude-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1698 ms | pass |
| claude-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1699 ms | pass |
| claude-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 163 ms | pass |
| claude-fd-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 156 ms | fail |
| claude-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 170 ms | pass |
| claude-fd-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 160 ms | fail |
| claude-fd-bare | wait | timed_out | 124 | yes | yes | none | yes | 157 ms | pass |
| claude-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 154 ms | pass |
| claude-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 156 ms | pass |
| claude-i-plain | wait | failed | 1 | yes | yes | none | yes | 211 ms | pass |
| claude-i-child | wait | failed | 1 | yes | yes | none | yes | 355 ms | pass |
| claude-i-setsid | wait | failed | 1 | yes | yes | none | yes | 337 ms | pass |
| claude-id-plain | wait | failed | 1 | yes | yes | none | yes | 231 ms | pass |
| claude-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 242 ms | pass |
| claude-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 302 ms | pass |
| claude-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1796 ms | pass |
| claude-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 188 ms | pass |
| claude-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 256 ms | pass |
| claude-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 152 ms | pass |
| claude-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 173 ms | pass |
| claude-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 160 ms | pass |
| claude-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1786 ms | pass |
| claude-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| claude-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| claude-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| claude-k-ignore | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| claude-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| claude-k-pipe | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| claude-k-orphan | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | leaf-setsid | n/a | 6 ms | pass |
| claude-k-orphan-late | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| claude-k-relay | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | relay-heartbeat | n/a | 6 ms | pass |
| claude-k-bare | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| claude-kk-plain | SIGKILL to delega and its keeper at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 6 ms | pass |
| claude-u-plain | wait | completed | 0 | yes | NO | none | yes | 54 ms | pass |
| claude-u-child | wait | completed | 0 | yes | NO | none | yes | 103 ms | pass |
| claude-u-setsid | wait | completed | 0 | yes | NO | leaf-setsid | yes | 123 ms | pass |
| claude-u-orphan | wait | completed | 0 | yes | NO | leaf-setsid | yes | 438 ms | pass |
| claude-u-bare | wait | completed | 0 | yes | NO | bare | yes | 422 ms | pass |
| claude-ud-plain | wait | timed_out | 124 | yes | NO | none | yes | 34 ms | pass |
| claude-ud-child | wait | timed_out | 124 | yes | NO | none | yes | 10 ms | pass |
| claude-ud-setsid | wait | timed_out | 124 | yes | NO | leaf-setsid | yes | 18 ms | pass |
| claude-ud-stubborn | wait | timed_out | 124 | yes | NO | none | yes | 1521 ms | pass |
| claude-ui-plain | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 24 ms | pass |
| claude-ui-child | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 29 ms | pass |
| claude-ui-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | NO | leaf-setsid | yes | 24 ms | pass |
| claude-ui-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 31 ms | pass |
| codex-a-plain | wait | completed | 0 | yes | yes | none | yes | 313 ms | pass |
| codex-a-child | wait | completed | 0 | yes | yes | none | yes | 423 ms | pass |
| codex-a-setsid | wait | completed | 0 | yes | yes | none | yes | 436 ms | pass |
| codex-a-ignore | wait | completed | 0 | yes | yes | none | yes | 2002 ms | pass |
| codex-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 317 ms | pass |
| codex-a-pipe | wait | completed | 0 | yes | yes | none | yes | 642 ms | pass |
| codex-a-orphan | wait | completed | 0 | yes | yes | leaf-setsid | NO | 1131 ms | fail |
| codex-a-orphan-late | wait | completed | 0 | yes | yes | none | yes | 839 ms | pass |
| codex-a-relay | wait | completed | 0 | yes | yes | relay-heartbeat | NO | 750 ms | fail |
| codex-a-bare | wait | completed | 0 | yes | yes | bare | NO | 3331 ms | fail |
| codex-b-plain | wait | failed | 1 | yes | yes | none | yes | 610 ms | pass |
| codex-b-child | wait | failed | 1 | yes | yes | none | yes | 733 ms | pass |
| codex-b-setsid | wait | failed | 1 | yes | yes | none | yes | 456 ms | pass |
| codex-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 168 ms | pass |
| codex-c-child | wait | timed_out | 124 | yes | yes | none | yes | 156 ms | pass |
| codex-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 173 ms | pass |
| codex-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1673 ms | pass |
| codex-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1721 ms | pass |
| codex-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 193 ms | pass |
| codex-c-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 174 ms | fail |
| codex-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 172 ms | pass |
| codex-c-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 155 ms | fail |
| codex-c-bare | wait | timed_out | 124 | yes | yes | none | yes | 282 ms | pass |
| codex-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 155 ms | pass |
| codex-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 150 ms | pass |
| codex-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 251 ms | pass |
| codex-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1687 ms | pass |
| codex-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1757 ms | pass |
| codex-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 246 ms | pass |
| codex-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | leaf-setsid | NO | 218 ms | fail |
| codex-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 225 ms | pass |
| codex-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | relay-heartbeat | NO | 222 ms | fail |
| codex-d-bare | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 225 ms | pass |
| codex-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 248 ms | pass |
| codex-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 166 ms | pass |
| codex-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 166 ms | pass |
| codex-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1717 ms | pass |
| codex-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 168 ms | pass |
| codex-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 167 ms | pass |
| codex-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 171 ms | pass |
| codex-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1708 ms | pass |
| codex-e-plain | wait | failed | 1 | yes | yes | none | yes | 880 ms | pass |
| codex-e-child | wait | failed | 1 | yes | yes | none | yes | 968 ms | pass |
| codex-e-setsid | wait | failed | 1 | yes | yes | none | yes | 968 ms | pass |
| codex-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2552 ms | pass |
| codex-e-pipe | wait | failed | 1 | yes | yes | none | yes | 971 ms | pass |
| codex-e-orphan | wait | failed | 1 | yes | yes | leaf-setsid | NO | 1279 ms | fail |
| codex-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 1009 ms | pass |
| codex-e-relay | wait | failed | 1 | yes | yes | relay-heartbeat | NO | 1081 ms | fail |
| codex-e-bare | wait | failed | 1 | yes | yes | none | yes | 2127 ms | pass |
| codex-ed-plain | wait | failed | 1 | yes | yes | none | yes | 1055 ms | pass |
| codex-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 1106 ms | pass |
| codex-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 143 ms | pass |
| codex-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 151 ms | pass |
| codex-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 149 ms | pass |
| codex-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1659 ms | pass |
| codex-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1719 ms | pass |
| codex-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 155 ms | pass |
| codex-fd-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 157 ms | fail |
| codex-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 176 ms | pass |
| codex-fd-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 175 ms | fail |
| codex-fd-bare | wait | timed_out | 124 | yes | yes | none | yes | 170 ms | pass |
| codex-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 178 ms | pass |
| codex-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 219 ms | pass |
| codex-i-plain | wait | failed | 1 | yes | yes | none | yes | 337 ms | pass |
| codex-i-child | wait | failed | 1 | yes | yes | none | yes | 471 ms | pass |
| codex-i-setsid | wait | failed | 1 | yes | yes | none | yes | 469 ms | pass |
| codex-id-plain | wait | failed | 1 | yes | yes | none | yes | 327 ms | pass |
| codex-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 354 ms | pass |
| codex-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 430 ms | pass |
| codex-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1882 ms | pass |
| codex-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 169 ms | pass |
| codex-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 238 ms | pass |
| codex-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 400 ms | pass |
| codex-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 474 ms | pass |
| codex-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 175 ms | pass |
| codex-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 2620 ms | pass |
| codex-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-ignore | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-pipe | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-orphan | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | leaf-setsid | n/a | 5 ms | pass |
| codex-k-orphan-late | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-relay | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | relay-heartbeat | n/a | 6 ms | pass |
| codex-k-bare | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-kk-plain | SIGKILL to delega and its keeper at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 6 ms | pass |
| codex-u-plain | wait | completed | 0 | yes | NO | none | yes | 70 ms | pass |
| codex-u-child | wait | completed | 0 | yes | NO | none | yes | 122 ms | pass |
| codex-u-setsid | wait | completed | 0 | yes | NO | leaf-setsid | yes | 123 ms | pass |
| codex-u-orphan | wait | completed | 0 | yes | NO | leaf-setsid | yes | 431 ms | pass |
| codex-u-bare | wait | completed | 0 | yes | NO | bare | yes | 437 ms | pass |
| codex-ud-plain | wait | timed_out | 124 | yes | NO | none | yes | 32 ms | pass |
| codex-ud-child | wait | timed_out | 124 | yes | NO | none | yes | 14 ms | pass |
| codex-ud-setsid | wait | timed_out | 124 | yes | NO | leaf-setsid | yes | 31 ms | pass |
| codex-ud-stubborn | wait | timed_out | 124 | yes | NO | none | yes | 1526 ms | pass |
| codex-ui-plain | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 28 ms | pass |
| codex-ui-child | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 29 ms | pass |
| codex-ui-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | NO | leaf-setsid | yes | 31 ms | pass |
| codex-ui-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 30 ms | pass |

## Live suite

Acceptance criterion 6 asks for a small live suite per provider. `--live` runs it against the
installed `claude` and `codex` (README.md, "Live suite"); this section is generated from the
recorded run in `live.jsonl`, whose first line is the run's metadata.

- Date: 2026-11-23.
- Machine: Darwin 27.2.0 arm64.
- delega: version 0.1.0-dev.
- Harness commit: `3eab6f8`.
- claude: claude 2.1.330.
- codex: codex 0.171.0.
- Claude cases used `--model haiku`, Codex cases `--effort low`.

18 cases: 18 pass, 0 fail, 0 not shown.

| Provider | Case | Version | Verdict | Outcome | Cleanup confirmed | Denials | Runs | Reported cost | Problems | Not shown and notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| claude | doctor | claude 2.1.330 | pass | - | n/a | 0 | 0 | - | none | 0 problems that block a run |
| claude | plain-answer | claude 2.1.330 | pass | completed | yes | 0 | 1 | $0.0001 | none | none |
| claude | read-cwd | claude 2.1.330 | pass | completed | yes | 0 | 1 | $0.0003 | none | none |
| claude | write-refused | claude 2.1.330 | pass | completed | yes | 0 | 1 | $0.0006 | none | 0 denials listed |
| claude | read-outside | claude 2.1.330 | pass | completed | yes | 1 | 1 | $0.0005 | none | none |
| claude | build | claude 2.1.330 | pass | completed | yes | 0 | 1 | $0.0012 | none | the temporary file was in Claude Code's per-user temporary directory |
| claude | resume | claude 2.1.330 | pass | completed | yes | 0 | 2 | $0.0003 | none | none |
| claude | interrupt | claude 2.1.330 | pass | interrupted | yes | 0 | 1 | - | none | none |
| claude | timeout | claude 2.1.330 | pass | timed_out | yes | 0 | 1 | - | none | none |
| codex | doctor | codex 0.171.0 | pass | - | n/a | 0 | 0 | - | none | 0 problems that block a run |
| codex | plain-answer | codex 0.171.0 | pass | completed | yes | 0 | 1 | - | none | none |
| codex | read-cwd | codex 0.171.0 | pass | completed | yes | 0 | 1 | - | none | none |
| codex | write-refused | codex 0.171.0 | pass | completed | yes | 0 | 1 | - | none | 0 denials listed |
| codex | read-outside | codex 0.171.0 | pass | completed | yes | 0 | 1 | - | none | the answer has the word seeded outside |
| codex | build | codex 0.171.0 | pass | completed | yes | 0 | 1 | - | none | the temporary file was in the TMPDIR the harness gave delega |
| codex | resume | codex 0.171.0 | pass | completed | yes | 0 | 2 | - | none | none |
| codex | interrupt | codex 0.171.0 | pass | interrupted | yes | 0 | 1 | - | none | none |
| codex | timeout | codex 0.171.0 | pass | timed_out | yes | 0 | 1 | - | none | none |

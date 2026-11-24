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
- Harness commit: `05a18fb`.
- 202 rows, 8 at a time, finished in 94 s.
- Fake providers only, a temporary `HOME` and a cleared environment per case; no real `claude` or `codex`.
- The harness samples for survivors at exit and after settling, using the process table, PID ledger, and relay heartbeat. It then kills any remaining processes.
- Cases run in parallel, so differences under about 100 ms mean nothing.

## Summary

- **Expected outcome and exit**: the outcome and the exit status the expectations table gives.
- **Cleanup verdict as expected**: `cleanup.confirmed` matches the fixture's reference value. The reference outcome, exit and cleanup verdict are recorded for comparison.
- **One stated outcome**: standard output held exactly one JSON document that parses and validates
as a `RunResult`, and `delega` exited without the harness having to kill it. Rows where the harness
closed standard output or killed `delega` have no document to read and are not counted.
- **Nothing left**: the harness observed no survivors after settling.
- **Cleanup consistency**: a Confirmed result had no observed survivor at exit or after settling. Counted over rows with a result outside the documented residuals.
- **Non-gap pass**: passing rows outside the documented residuals. All rows are checked for the guarantees that apply to them.
- **Gap shown** and **Gap not shown**: observations of the listed survivor roles after settling, independent of the verdict.

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Cleanup consistency | Pass | Non-gap pass | Gap shown | Gap not shown | Skipped | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| all | 202 | 202 | 202 | 172 of 172 | 164 | 150 of 150 | 202 | 174 | 28 | 0 | 0 | 0 |
| claude | 101 | 101 | 101 | 86 of 86 | 82 | 75 of 75 | 101 | 87 | 14 | 0 | 0 | 0 |
| codex | 101 | 101 | 101 | 86 of 86 | 82 | 75 of 75 | 101 | 87 | 14 | 0 | 0 | 0 |

### By caller action

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Cleanup consistency | Pass | Non-gap pass | Gap shown | Gap not shown | Skipped | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| provider ends by itself | 56 | 56 | 56 | 56 of 56 | 46 | 46 of 46 | 56 | 46 | 10 | 0 | 0 | 0 |
| time limit (--timeout) | 50 | 50 | 50 | 50 of 50 | 42 | 42 of 42 | 50 | 42 | 8 | 0 | 0 | 0 |
| SIGINT | 24 | 24 | 24 | 24 of 24 | 20 | 20 of 20 | 24 | 20 | 4 | 0 | 0 | 0 |
| SIGTERM | 8 | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 8 | 0 | 0 | 0 | 0 |
| SIGHUP | 8 | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 8 | 0 | 0 | 0 | 0 |
| stdout closed early | 8 | 8 | 8 | 0 of 0 | 8 | 0 of 0 | 8 | 8 | 0 | 0 | 0 | 0 |
| SIGKILL of delega | 20 | 20 | 20 | 0 of 0 | 16 | 0 of 0 | 20 | 16 | 4 | 0 | 0 | 0 |
| SIGKILL of delega and its keeper | 2 | 2 | 2 | 0 of 0 | 0 | 0 of 0 | 2 | 0 | 2 | 0 | 0 | 0 |
| other | 26 | 26 | 26 | 26 of 26 | 16 | 26 of 26 | 26 | 26 | 0 | 0 | 0 | 0 |

### By variant (what the provider leaves running)

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Cleanup consistency | Pass | Non-gap pass | Gap shown | Gap not shown | Skipped | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| plain | 42 | 42 | 42 | 34 of 34 | 40 | 34 of 34 | 42 | 40 | 2 | 0 | 0 | 0 |
| child | 34 | 34 | 34 | 30 of 30 | 34 | 30 of 30 | 34 | 34 | 0 | 0 | 0 | 0 |
| pipe | 14 | 14 | 14 | 12 of 12 | 14 | 12 of 12 | 14 | 14 | 0 | 0 | 0 | 0 |
| ignore | 16 | 16 | 16 | 14 of 14 | 16 | 14 of 14 | 16 | 16 | 0 | 0 | 0 | 0 |
| stubborn | 16 | 16 | 16 | 14 of 14 | 16 | 14 of 14 | 16 | 16 | 0 | 0 | 0 | 0 |
| setsid | 28 | 28 | 28 | 24 of 24 | 22 | 24 of 24 | 28 | 28 | 0 | 0 | 0 | 0 |
| orphan | 14 | 14 | 14 | 12 of 12 | 0 | 2 of 2 | 14 | 2 | 12 | 0 | 0 | 0 |
| orphan-late | 12 | 12 | 12 | 10 of 10 | 12 | 10 of 10 | 12 | 12 | 0 | 0 | 0 | 0 |
| relay | 12 | 12 | 12 | 10 of 10 | 0 | 0 of 0 | 12 | 0 | 12 | 0 | 0 | 0 |

## Latency

Measured from the harness's action to `delega` exiting (a signal), past the `--timeout` (a time
limit that fired), or from start to exit (a run left to finish). A process that ignores TERM adds the
grace period before SIGKILL. SIGKILL rows are left out.

| Measure | Rows | p50 | p90 | p99 | max |
| --- | --- | --- | --- | --- | --- |
| signal to exit | 40 | 201 ms | 252 ms | 326 ms | 326 ms |
| signal to exit, a process ignores TERM | 10 | 1713 ms | 1789 ms | 1799 ms | 1799 ms |
| timeout overshoot | 52 | 180 ms | 288 ms | 411 ms | 411 ms |
| timeout overshoot, a process ignores TERM | 12 | 1725 ms | 1795 ms | 1801 ms | 1801 ms |
| run to exit | 62 | 724 ms | 1519 ms | 3586 ms | 3586 ms |
| run to exit, a process ignores TERM | 6 | 1981 ms | 2623 ms | 2623 ms | 2623 ms |

## Failures and skips

None.

## Gap observations

The matrix's `GAPS` table classifies documented residuals for the recorded platform. A gap is shown when all its listed survivor roles are observed after settling. These rows retain the applicable result and termination checks. The presence or absence of survivors is informational.

### Documented gap: 2 rows, 2 shown in this run

SIGKILL of delega and of its keeper together: no code of either runs, so nothing stops the worker or enforces its time limit. The accepted residual on every platform (process module docs, "What remains possible").

| Row | Outcome | Exit | Cleanup confirmed | Survivors at exit | Survivors after settling | Gap observation | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- |
| claude-kk-plain | none | signal 9 | n/a | provider | provider | gap shown | pass |
| codex-kk-plain | none | signal 9 | n/a | provider | provider | gap shown | pass |

### Documented gap: 14 rows, 14 shown in this run

macOS residual on a system that hides the environment of its own programs from `ps -E` (System Integrity Protection does): the process left the provider's session and its parent is gone, so no listing ties it to the run, macOS has no subreaper, and the run marker is not visible because these fakes run sh, sleep and perl. Where the environment is visible the marker can find it. Linux closes this through the keeper (process module docs, "What remains possible").

| Row | Outcome | Exit | Cleanup confirmed | Survivors at exit | Survivors after settling | Gap observation | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- |
| claude-a-orphan | completed | 0 | yes | leaf-setsid | leaf-setsid | gap shown | pass |
| claude-a-bare | completed | 0 | yes | bare | bare | gap shown | pass |
| claude-c-orphan | timed_out | 124 | yes | leaf-setsid | leaf-setsid | gap shown | pass |
| claude-d-orphan | interrupted | 130 | yes | leaf-setsid | leaf-setsid | gap shown | pass |
| claude-e-orphan | failed | 1 | yes | leaf-setsid | leaf-setsid | gap shown | pass |
| claude-fd-orphan | timed_out | 124 | yes | leaf-setsid | leaf-setsid | gap shown | pass |
| claude-k-orphan | none | signal 9 | n/a | leaf-setsid | leaf-setsid | gap shown | pass |
| codex-a-orphan | completed | 0 | yes | leaf-setsid | leaf-setsid | gap shown | pass |
| codex-a-bare | completed | 0 | yes | bare | bare | gap shown | pass |
| codex-c-orphan | timed_out | 124 | yes | leaf-setsid | leaf-setsid | gap shown | pass |
| codex-d-orphan | interrupted | 130 | yes | leaf-setsid | leaf-setsid | gap shown | pass |
| codex-e-orphan | failed | 1 | yes | leaf-setsid | leaf-setsid | gap shown | pass |
| codex-fd-orphan | timed_out | 124 | yes | leaf-setsid | leaf-setsid | gap shown | pass |
| codex-k-orphan | none | signal 9 | n/a | leaf-setsid | leaf-setsid | gap shown | pass |

### Documented gap: 12 rows, 12 shown in this run

macOS residual: a fast fork relay can leave each listing before cleanup signals it. Linux closes this through the keeper's own walk of its tree (process module docs, "What remains possible").

| Row | Outcome | Exit | Cleanup confirmed | Survivors at exit | Survivors after settling | Gap observation | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- |
| claude-a-relay | completed | 0 | yes | relay-heartbeat | relay-heartbeat | gap shown | pass |
| claude-c-relay | timed_out | 124 | yes | relay-heartbeat | relay-heartbeat | gap shown | pass |
| claude-d-relay | interrupted | 130 | yes | relay-heartbeat | relay-heartbeat | gap shown | pass |
| claude-e-relay | failed | 1 | yes | relay-heartbeat | relay-heartbeat | gap shown | pass |
| claude-fd-relay | timed_out | 124 | yes | relay-heartbeat | relay-heartbeat | gap shown | pass |
| claude-k-relay | none | signal 9 | n/a | relay-heartbeat | relay-heartbeat | gap shown | pass |
| codex-a-relay | completed | 0 | yes | relay-heartbeat | relay-heartbeat | gap shown | pass |
| codex-c-relay | timed_out | 124 | yes | relay-heartbeat | relay-heartbeat | gap shown | pass |
| codex-d-relay | interrupted | 130 | yes | relay-heartbeat | relay-heartbeat | gap shown | pass |
| codex-e-relay | failed | 1 | yes | relay-heartbeat | relay-heartbeat | gap shown | pass |
| codex-fd-relay | timed_out | 124 | yes | relay-heartbeat | relay-heartbeat | gap shown | pass |
| codex-k-relay | none | signal 9 | n/a | relay-heartbeat | relay-heartbeat | gap shown | pass |

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

| Run | Pass | Non-gap pass | Gap shown | Gap not shown | Skipped | Fail | p99 latency (not SIGKILL) | Max latency |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 202 | 174 | 28 | 0 | 0 | 0 | 3036 ms | 3591 ms |
| 2 | 202 | 174 | 28 | 0 | 0 | 0 | 3399 ms | 3522 ms |
| 3 | 202 | 174 | 28 | 0 | 0 | 0 | 3221 ms | 3423 ms |

No row changed verdict, outcome, exit status, survivors or cleanup report between runs.

## All rows

| Row | Action | Outcome | Exit | One document | Cleanup confirmed | Left running | Cleanup consistency | Latency | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| claude-a-plain | wait | completed | 0 | yes | yes | none | yes | 610 ms | pass |
| claude-a-child | wait | completed | 0 | yes | yes | none | yes | 754 ms | pass |
| claude-a-setsid | wait | completed | 0 | yes | yes | none | yes | 739 ms | pass |
| claude-a-ignore | wait | completed | 0 | yes | yes | none | yes | 2248 ms | pass |
| claude-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 577 ms | pass |
| claude-a-pipe | wait | completed | 0 | yes | yes | none | yes | 739 ms | pass |
| claude-a-orphan | wait | completed | 0 | yes | yes | leaf-setsid | n/a | 1012 ms | pass |
| claude-a-orphan-late | wait | completed | 0 | yes | yes | none | yes | 724 ms | pass |
| claude-a-relay | wait | completed | 0 | yes | yes | relay-heartbeat | n/a | 397 ms | pass |
| claude-a-bare | wait | completed | 0 | yes | yes | bare | n/a | 2854 ms | pass |
| claude-b-plain | wait | failed | 1 | yes | yes | none | yes | 253 ms | pass |
| claude-b-child | wait | failed | 1 | yes | yes | none | yes | 388 ms | pass |
| claude-b-setsid | wait | failed | 1 | yes | yes | none | yes | 377 ms | pass |
| claude-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 182 ms | pass |
| claude-c-child | wait | timed_out | 124 | yes | yes | none | yes | 161 ms | pass |
| claude-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 146 ms | pass |
| claude-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1682 ms | pass |
| claude-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1713 ms | pass |
| claude-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 180 ms | pass |
| claude-c-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | n/a | 177 ms | pass |
| claude-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 165 ms | pass |
| claude-c-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | n/a | 153 ms | pass |
| claude-c-bare | wait | timed_out | 124 | yes | yes | none | yes | 383 ms | pass |
| claude-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 193 ms | pass |
| claude-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 305 ms | pass |
| claude-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 251 ms | pass |
| claude-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1743 ms | pass |
| claude-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1778 ms | pass |
| claude-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 326 ms | pass |
| claude-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | leaf-setsid | n/a | 280 ms | pass |
| claude-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 259 ms | pass |
| claude-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | relay-heartbeat | n/a | 237 ms | pass |
| claude-d-bare | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 201 ms | pass |
| claude-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 193 ms | pass |
| claude-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 188 ms | pass |
| claude-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 172 ms | pass |
| claude-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1696 ms | pass |
| claude-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 167 ms | pass |
| claude-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 153 ms | pass |
| claude-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 165 ms | pass |
| claude-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1704 ms | pass |
| claude-e-plain | wait | failed | 1 | yes | yes | none | yes | 818 ms | pass |
| claude-e-child | wait | failed | 1 | yes | yes | none | yes | 851 ms | pass |
| claude-e-setsid | wait | failed | 1 | yes | yes | none | yes | 889 ms | pass |
| claude-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2365 ms | pass |
| claude-e-pipe | wait | failed | 1 | yes | yes | none | yes | 859 ms | pass |
| claude-e-orphan | wait | failed | 1 | yes | yes | leaf-setsid | n/a | 1158 ms | pass |
| claude-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 864 ms | pass |
| claude-e-relay | wait | failed | 1 | yes | yes | relay-heartbeat | n/a | 861 ms | pass |
| claude-e-bare | wait | failed | 1 | yes | yes | none | yes | 1852 ms | pass |
| claude-ed-plain | wait | failed | 1 | yes | yes | none | yes | 784 ms | pass |
| claude-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 840 ms | pass |
| claude-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 140 ms | pass |
| claude-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 154 ms | pass |
| claude-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 172 ms | pass |
| claude-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1801 ms | pass |
| claude-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1795 ms | pass |
| claude-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 332 ms | pass |
| claude-fd-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | n/a | 411 ms | pass |
| claude-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 227 ms | pass |
| claude-fd-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | n/a | 150 ms | pass |
| claude-fd-bare | wait | timed_out | 124 | yes | yes | none | yes | 150 ms | pass |
| claude-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 162 ms | pass |
| claude-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 180 ms | pass |
| claude-i-plain | wait | failed | 1 | yes | yes | none | yes | 298 ms | pass |
| claude-i-child | wait | failed | 1 | yes | yes | none | yes | 331 ms | pass |
| claude-i-setsid | wait | failed | 1 | yes | yes | none | yes | 318 ms | pass |
| claude-id-plain | wait | failed | 1 | yes | yes | none | yes | 194 ms | pass |
| claude-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 251 ms | pass |
| claude-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 315 ms | pass |
| claude-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1795 ms | pass |
| claude-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 167 ms | pass |
| claude-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 235 ms | pass |
| claude-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 151 ms | pass |
| claude-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 160 ms | pass |
| claude-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 169 ms | pass |
| claude-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1797 ms | pass |
| claude-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| claude-k-ignore | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| claude-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-pipe | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-orphan | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | leaf-setsid | n/a | 6 ms | pass |
| claude-k-orphan-late | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-relay | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | relay-heartbeat | n/a | 5 ms | pass |
| claude-k-bare | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| claude-kk-plain | SIGKILL to delega and its keeper at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 7 ms | pass |
| claude-u-plain | wait | completed | 0 | yes | NO | none | yes | 57 ms | pass |
| claude-u-child | wait | completed | 0 | yes | NO | none | yes | 110 ms | pass |
| claude-u-setsid | wait | completed | 0 | yes | NO | leaf-setsid | yes | 118 ms | pass |
| claude-u-orphan | wait | completed | 0 | yes | NO | leaf-setsid | yes | 424 ms | pass |
| claude-u-bare | wait | completed | 0 | yes | NO | bare | yes | 419 ms | pass |
| claude-ud-plain | wait | timed_out | 124 | yes | NO | none | yes | 30 ms | pass |
| claude-ud-child | wait | timed_out | 124 | yes | NO | none | yes | 31 ms | pass |
| claude-ud-setsid | wait | timed_out | 124 | yes | NO | leaf-setsid | yes | 38 ms | pass |
| claude-ud-stubborn | wait | timed_out | 124 | yes | NO | none | yes | 1525 ms | pass |
| claude-ui-plain | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 29 ms | pass |
| claude-ui-child | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 34 ms | pass |
| claude-ui-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | NO | leaf-setsid | yes | 33 ms | pass |
| claude-ui-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 30 ms | pass |
| codex-a-plain | wait | completed | 0 | yes | yes | none | yes | 309 ms | pass |
| codex-a-child | wait | completed | 0 | yes | yes | none | yes | 437 ms | pass |
| codex-a-setsid | wait | completed | 0 | yes | yes | none | yes | 444 ms | pass |
| codex-a-ignore | wait | completed | 0 | yes | yes | none | yes | 1981 ms | pass |
| codex-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 319 ms | pass |
| codex-a-pipe | wait | completed | 0 | yes | yes | none | yes | 477 ms | pass |
| codex-a-orphan | wait | completed | 0 | yes | yes | leaf-setsid | n/a | 927 ms | pass |
| codex-a-orphan-late | wait | completed | 0 | yes | yes | none | yes | 689 ms | pass |
| codex-a-relay | wait | completed | 0 | yes | yes | relay-heartbeat | n/a | 701 ms | pass |
| codex-a-bare | wait | completed | 0 | yes | yes | bare | n/a | 3586 ms | pass |
| codex-b-plain | wait | failed | 1 | yes | yes | none | yes | 549 ms | pass |
| codex-b-child | wait | failed | 1 | yes | yes | none | yes | 679 ms | pass |
| codex-b-setsid | wait | failed | 1 | yes | yes | none | yes | 526 ms | pass |
| codex-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 248 ms | pass |
| codex-c-child | wait | timed_out | 124 | yes | yes | none | yes | 279 ms | pass |
| codex-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 337 ms | pass |
| codex-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1725 ms | pass |
| codex-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1786 ms | pass |
| codex-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 279 ms | pass |
| codex-c-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | n/a | 193 ms | pass |
| codex-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 200 ms | pass |
| codex-c-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | n/a | 288 ms | pass |
| codex-c-bare | wait | timed_out | 124 | yes | yes | none | yes | 228 ms | pass |
| codex-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 215 ms | pass |
| codex-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 251 ms | pass |
| codex-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 198 ms | pass |
| codex-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1713 ms | pass |
| codex-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1799 ms | pass |
| codex-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 250 ms | pass |
| codex-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | leaf-setsid | n/a | 252 ms | pass |
| codex-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 224 ms | pass |
| codex-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | relay-heartbeat | n/a | 222 ms | pass |
| codex-d-bare | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 220 ms | pass |
| codex-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 244 ms | pass |
| codex-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 230 ms | pass |
| codex-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 233 ms | pass |
| codex-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1789 ms | pass |
| codex-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 227 ms | pass |
| codex-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 226 ms | pass |
| codex-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 199 ms | pass |
| codex-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1747 ms | pass |
| codex-e-plain | wait | failed | 1 | yes | yes | none | yes | 1060 ms | pass |
| codex-e-child | wait | failed | 1 | yes | yes | none | yes | 1079 ms | pass |
| codex-e-setsid | wait | failed | 1 | yes | yes | none | yes | 1192 ms | pass |
| codex-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2623 ms | pass |
| codex-e-pipe | wait | failed | 1 | yes | yes | none | yes | 1145 ms | pass |
| codex-e-orphan | wait | failed | 1 | yes | yes | leaf-setsid | n/a | 1519 ms | pass |
| codex-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 1280 ms | pass |
| codex-e-relay | wait | failed | 1 | yes | yes | relay-heartbeat | n/a | 1404 ms | pass |
| codex-e-bare | wait | failed | 1 | yes | yes | none | yes | 2219 ms | pass |
| codex-ed-plain | wait | failed | 1 | yes | yes | none | yes | 972 ms | pass |
| codex-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 990 ms | pass |
| codex-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 174 ms | pass |
| codex-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 144 ms | pass |
| codex-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 154 ms | pass |
| codex-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1683 ms | pass |
| codex-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1754 ms | pass |
| codex-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 198 ms | pass |
| codex-fd-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | n/a | 182 ms | pass |
| codex-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 213 ms | pass |
| codex-fd-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | n/a | 266 ms | pass |
| codex-fd-bare | wait | timed_out | 124 | yes | yes | none | yes | 248 ms | pass |
| codex-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 220 ms | pass |
| codex-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 161 ms | pass |
| codex-i-plain | wait | failed | 1 | yes | yes | none | yes | 424 ms | pass |
| codex-i-child | wait | failed | 1 | yes | yes | none | yes | 762 ms | pass |
| codex-i-setsid | wait | failed | 1 | yes | yes | none | yes | 861 ms | pass |
| codex-id-plain | wait | failed | 1 | yes | yes | none | yes | 590 ms | pass |
| codex-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 201 ms | pass |
| codex-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 259 ms | pass |
| codex-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1763 ms | pass |
| codex-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 192 ms | pass |
| codex-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 231 ms | pass |
| codex-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 180 ms | pass |
| codex-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 173 ms | pass |
| codex-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 224 ms | pass |
| codex-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1936 ms | pass |
| codex-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-ignore | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| codex-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-pipe | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-orphan | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | leaf-setsid | n/a | 5 ms | pass |
| codex-k-orphan-late | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| codex-k-relay | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | relay-heartbeat | n/a | 7 ms | pass |
| codex-k-bare | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-kk-plain | SIGKILL to delega and its keeper at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 7 ms | pass |
| codex-u-plain | wait | completed | 0 | yes | NO | none | yes | 90 ms | pass |
| codex-u-child | wait | completed | 0 | yes | NO | none | yes | 128 ms | pass |
| codex-u-setsid | wait | completed | 0 | yes | NO | leaf-setsid | yes | 137 ms | pass |
| codex-u-orphan | wait | completed | 0 | yes | NO | leaf-setsid | yes | 441 ms | pass |
| codex-u-bare | wait | completed | 0 | yes | NO | bare | yes | 445 ms | pass |
| codex-ud-plain | wait | timed_out | 124 | yes | NO | none | yes | 30 ms | pass |
| codex-ud-child | wait | timed_out | 124 | yes | NO | none | yes | 41 ms | pass |
| codex-ud-setsid | wait | timed_out | 124 | yes | NO | leaf-setsid | yes | 24 ms | pass |
| codex-ud-stubborn | wait | timed_out | 124 | yes | NO | none | yes | 1538 ms | pass |
| codex-ui-plain | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 28 ms | pass |
| codex-ui-child | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 35 ms | pass |
| codex-ui-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | NO | leaf-setsid | yes | 35 ms | pass |
| codex-ui-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 36 ms | pass |

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

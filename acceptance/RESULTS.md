# Results: does delega meet criteria 1 and 2?

Generated from `results.jsonl` by `cargo run -p delega-acceptance -- --report`; do not edit by hand.
The harness is described in [README.md](README.md). Criteria, from the repository README:
(1) every run ends in exactly one stated outcome, never a hang and never silence; (2) after any
outcome the provider and its descendants are gone, or the result says cleanup could not be
confirmed and what remains.

## Environment

- Machine: Darwin 27.2.0 arm64.
- delega: version 0.1.0-dev.
- Harness commit: `74f6f9b`.
- 202 rows, 8 at a time, finished in 92 s.
- Fake providers only, a temporary `HOME` and a cleared environment per case; no real `claude` or `codex`.
- Survivors are measured by a per-case marker in the process table and by a heartbeat file, 1 s after
`delega` exits, whatever `delega` reported; the harness then kills them.
- Cases run in parallel, so differences under about 100 ms mean nothing.

## Summary

- **Expected outcome and exit**: the outcome and the exit status the expectations table gives.
- **Cleanup verdict as expected**: `cleanup.confirmed` is what the expectations table gives (true on
every row except the rows that make process listing unavailable, where it must be false; for rows
with no result, there is none).
- **One stated outcome**: standard output held exactly one JSON document that parses and validates
as a `RunResult`, and `delega` exited without the harness having to kill it. Rows where the harness
closed standard output or killed `delega` have no document to read and are not counted.
- **Nothing left**: no process carrying the case marker was alive 1 s after `delega` exited.
- **Honest report**: survivors exist only when `cleanup.confirmed` is false. Counted over rows that
have a result.

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Honest report | Pass | Expected failure | Gap not shown | Unexpected pass | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| all | 202 | 202 | 202 | 172 of 172 | 164 | 150 of 172 | 174 | 28 | 0 | 0 | 0 |
| claude | 101 | 101 | 101 | 86 of 86 | 82 | 75 of 86 | 87 | 14 | 0 | 0 | 0 |
| codex | 101 | 101 | 101 | 86 of 86 | 82 | 75 of 86 | 87 | 14 | 0 | 0 | 0 |

### By caller action

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Honest report | Pass | Expected failure | Gap not shown | Unexpected pass | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| provider ends by itself | 56 | 56 | 56 | 56 of 56 | 46 | 46 of 56 | 46 | 10 | 0 | 0 | 0 |
| time limit (--timeout) | 50 | 50 | 50 | 50 of 50 | 42 | 42 of 50 | 42 | 8 | 0 | 0 | 0 |
| SIGINT | 24 | 24 | 24 | 24 of 24 | 20 | 20 of 24 | 20 | 4 | 0 | 0 | 0 |
| SIGTERM | 8 | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 0 | 0 | 0 | 0 |
| SIGHUP | 8 | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 0 | 0 | 0 | 0 |
| stdout closed early | 8 | 8 | 8 | 0 of 0 | 8 | 0 of 0 | 8 | 0 | 0 | 0 | 0 |
| SIGKILL of delega | 20 | 20 | 20 | 0 of 0 | 16 | 0 of 0 | 16 | 4 | 0 | 0 | 0 |
| SIGKILL of delega and its keeper | 2 | 2 | 2 | 0 of 0 | 0 | 0 of 0 | 0 | 2 | 0 | 0 | 0 |
| other | 26 | 26 | 26 | 26 of 26 | 16 | 26 of 26 | 26 | 0 | 0 | 0 | 0 |

### By variant (what the provider leaves running)

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Honest report | Pass | Expected failure | Gap not shown | Unexpected pass | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| plain | 42 | 42 | 42 | 34 of 34 | 40 | 34 of 34 | 40 | 2 | 0 | 0 | 0 |
| child | 34 | 34 | 34 | 30 of 30 | 34 | 30 of 30 | 34 | 0 | 0 | 0 | 0 |
| pipe | 14 | 14 | 14 | 12 of 12 | 14 | 12 of 12 | 14 | 0 | 0 | 0 | 0 |
| ignore | 16 | 16 | 16 | 14 of 14 | 16 | 14 of 14 | 16 | 0 | 0 | 0 | 0 |
| stubborn | 16 | 16 | 16 | 14 of 14 | 16 | 14 of 14 | 16 | 0 | 0 | 0 | 0 |
| setsid | 28 | 28 | 28 | 24 of 24 | 22 | 24 of 24 | 28 | 0 | 0 | 0 | 0 |
| orphan | 14 | 14 | 14 | 12 of 12 | 0 | 2 of 12 | 2 | 12 | 0 | 0 | 0 |
| orphan-late | 12 | 12 | 12 | 10 of 10 | 12 | 10 of 10 | 12 | 0 | 0 | 0 | 0 |
| relay | 12 | 12 | 12 | 10 of 10 | 0 | 0 of 10 | 0 | 12 | 0 | 0 | 0 |

## Latency

Measured from the harness's action to `delega` exiting (a signal), past the `--timeout` (a time
limit that fired), or from start to exit (a run left to finish). A process that ignores TERM adds the
grace period before SIGKILL. SIGKILL rows are left out.

| Measure | Rows | p50 | p90 | p99 | max |
| --- | --- | --- | --- | --- | --- |
| signal to exit | 40 | 146 ms | 179 ms | 234 ms | 234 ms |
| signal to exit, a process ignores TERM | 10 | 1685 ms | 1701 ms | 1702 ms | 1702 ms |
| timeout overshoot | 52 | 154 ms | 170 ms | 294 ms | 294 ms |
| timeout overshoot, a process ignores TERM | 12 | 1670 ms | 1749 ms | 1776 ms | 1776 ms |
| run to exit | 62 | 507 ms | 1230 ms | 3539 ms | 3539 ms |
| run to exit, a process ignores TERM | 6 | 1980 ms | 2410 ms | 2410 ms | 2410 ms |

## Failures that are not expected

None. Every row either met its expectation or is a listed expected failure.

## Expected failures

Rows in the expectations table (`acceptance/src/matrix.rs`) that are expected to fail criterion 2.
Each pins exactly which roles may survive, and fails the run if anything else survives or the
outcome, exit status or cleanup verdict differs. A row in this list that passes fails the run, so
the table is corrected when a gap closes. There are three classes, listed separately:

- **documented gap**: the process module's docs list it as known.
- **undocumented gap**: a failure this harness found that the docs do not describe.
- **defect**: a failure that is neither.

- documented gap: 28 rows.
- undocumented gap: 0 rows.
- defect: 0 rows.

### documented gap: 2 rows, 2 failed as expected in this run

SIGKILL of delega and of its keeper together: no code of either runs, so nothing stops the worker or enforces its time limit. The accepted residual on every platform (process module docs, "What remains possible").

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest |
| --- | --- | --- | --- | --- | --- |
| claude-kk-plain | none | signal 9 | n/a | provider | n/a |
| codex-kk-plain | none | signal 9 | n/a | provider | n/a |

### documented gap: 12 rows, 12 failed as expected in this run

macOS residual: a fast fork relay is never in a process listing for long enough to be stopped, and the freeze of listings stopped the process-layer relay in 0 of 50 runs on macOS 27.2. Linux closes this through the keeper's own walk of its tree (process module docs, "What remains possible").

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest |
| --- | --- | --- | --- | --- | --- |
| claude-a-relay | completed | 0 | yes | relay-heartbeat | NO |
| claude-c-relay | timed_out | 124 | yes | relay-heartbeat | NO |
| claude-d-relay | interrupted | 130 | yes | relay-heartbeat | NO |
| claude-e-relay | failed | 1 | yes | relay-heartbeat | NO |
| claude-fd-relay | timed_out | 124 | yes | relay-heartbeat | NO |
| claude-k-relay | none | signal 9 | n/a | relay-heartbeat | n/a |
| codex-a-relay | completed | 0 | yes | relay-heartbeat | NO |
| codex-c-relay | timed_out | 124 | yes | relay-heartbeat | NO |
| codex-d-relay | interrupted | 130 | yes | relay-heartbeat | NO |
| codex-e-relay | failed | 1 | yes | relay-heartbeat | NO |
| codex-fd-relay | timed_out | 124 | yes | relay-heartbeat | NO |
| codex-k-relay | none | signal 9 | n/a | relay-heartbeat | n/a |

### documented gap: 14 rows, 14 failed as expected in this run

macOS residual: the process left the provider's session and its parent is gone, so no listing ties it to the run, and macOS has no subreaper. The run marker would find it, but `ps -E` shows no environment for Apple's own programs (these fakes run sh, sleep and perl), measured on macOS 27.2 with SIP enabled. Linux closes this through the keeper (process module docs, "What remains possible").

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest |
| --- | --- | --- | --- | --- | --- |
| claude-a-orphan | completed | 0 | yes | leaf-setsid | NO |
| claude-a-bare | completed | 0 | yes | bare | NO |
| claude-c-orphan | timed_out | 124 | yes | leaf-setsid | NO |
| claude-d-orphan | interrupted | 130 | yes | leaf-setsid | NO |
| claude-e-orphan | failed | 1 | yes | leaf-setsid | NO |
| claude-fd-orphan | timed_out | 124 | yes | leaf-setsid | NO |
| claude-k-orphan | none | signal 9 | n/a | leaf-setsid | n/a |
| codex-a-orphan | completed | 0 | yes | leaf-setsid | NO |
| codex-a-bare | completed | 0 | yes | bare | NO |
| codex-c-orphan | timed_out | 124 | yes | leaf-setsid | NO |
| codex-d-orphan | interrupted | 130 | yes | leaf-setsid | NO |
| codex-e-orphan | failed | 1 | yes | leaf-setsid | NO |
| codex-fd-orphan | timed_out | 124 | yes | leaf-setsid | NO |
| codex-k-orphan | none | signal 9 | n/a | leaf-setsid | n/a |

## Rows that force an unconfirmed cleanup

Process listing is made unavailable (`DELEGA_TEST_NO_PROCESS_LISTING` set, a test-only seam in
`delega-core`). Each row must report `cleanup.confirmed` false with a reason that contains
"process inspection was unavailable" and says what could not be checked. With no listing it cannot
name a descendant in a session of its own, so `pids` and `process_groups` hold only what is known
to be alive (nothing, once the worker's group is gone). Such a descendant survives and the report
says cleanup was not confirmed, which meets criterion 2 by its second clause but names nothing.

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

| Run | Pass | Expected failure | Gap not shown | Unexpected pass | Fail | p99 latency (not SIGKILL) | Max latency |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 174 | 28 | 0 | 0 | 0 | 3006 ms | 3094 ms |
| 2 | 174 | 27 | 1 | 0 | 0 | 3572 ms | 3588 ms |
| 3 | 174 | 28 | 0 | 0 | 0 | 3027 ms | 3686 ms |

Rows whose result changed between runs:

| Row | Per run: verdict, outcome, survivors |
| --- | --- |
| codex-a-bare | #1 expected_failure, completed, bare; #2 gap_not_shown, completed, none; #3 expected_failure, completed, bare |

## All rows

| Row | Action | Outcome | Exit | One document | Cleanup confirmed | Left running | Honest | Latency | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| claude-a-plain | wait | completed | 0 | yes | yes | none | yes | 687 ms | pass |
| claude-a-child | wait | completed | 0 | yes | yes | none | yes | 848 ms | pass |
| claude-a-setsid | wait | completed | 0 | yes | yes | none | yes | 820 ms | pass |
| claude-a-ignore | wait | completed | 0 | yes | yes | none | yes | 2327 ms | pass |
| claude-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 684 ms | pass |
| claude-a-pipe | wait | completed | 0 | yes | yes | none | yes | 842 ms | pass |
| claude-a-orphan | wait | completed | 0 | yes | yes | leaf-setsid | NO | 1119 ms | expected_failure |
| claude-a-orphan-late | wait | completed | 0 | yes | yes | none | yes | 822 ms | pass |
| claude-a-relay | wait | completed | 0 | yes | yes | relay-heartbeat | NO | 342 ms | expected_failure |
| claude-a-bare | wait | completed | 0 | yes | yes | bare | NO | 2846 ms | expected_failure |
| claude-b-plain | wait | failed | 1 | yes | yes | none | yes | 233 ms | pass |
| claude-b-child | wait | failed | 1 | yes | yes | none | yes | 361 ms | pass |
| claude-b-setsid | wait | failed | 1 | yes | yes | none | yes | 351 ms | pass |
| claude-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 166 ms | pass |
| claude-c-child | wait | timed_out | 124 | yes | yes | none | yes | 158 ms | pass |
| claude-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 153 ms | pass |
| claude-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1666 ms | pass |
| claude-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1701 ms | pass |
| claude-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 158 ms | pass |
| claude-c-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 154 ms | expected_failure |
| claude-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 140 ms | pass |
| claude-c-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 146 ms | expected_failure |
| claude-c-bare | wait | timed_out | 124 | yes | yes | none | yes | 146 ms | pass |
| claude-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 140 ms | pass |
| claude-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 160 ms | pass |
| claude-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 166 ms | pass |
| claude-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1648 ms | pass |
| claude-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1682 ms | pass |
| claude-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 155 ms | pass |
| claude-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | leaf-setsid | NO | 166 ms | expected_failure |
| claude-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 146 ms | pass |
| claude-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | relay-heartbeat | NO | 151 ms | expected_failure |
| claude-d-bare | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 148 ms | pass |
| claude-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 160 ms | pass |
| claude-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 142 ms | pass |
| claude-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 159 ms | pass |
| claude-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1692 ms | pass |
| claude-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 130 ms | pass |
| claude-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 136 ms | pass |
| claude-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 161 ms | pass |
| claude-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1701 ms | pass |
| claude-e-plain | wait | failed | 1 | yes | yes | none | yes | 772 ms | pass |
| claude-e-child | wait | failed | 1 | yes | yes | none | yes | 820 ms | pass |
| claude-e-setsid | wait | failed | 1 | yes | yes | none | yes | 833 ms | pass |
| claude-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2322 ms | pass |
| claude-e-pipe | wait | failed | 1 | yes | yes | none | yes | 843 ms | pass |
| claude-e-orphan | wait | failed | 1 | yes | yes | leaf-setsid | NO | 1180 ms | expected_failure |
| claude-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 847 ms | pass |
| claude-e-relay | wait | failed | 1 | yes | yes | relay-heartbeat | NO | 850 ms | expected_failure |
| claude-e-bare | wait | failed | 1 | yes | yes | none | yes | 1828 ms | pass |
| claude-ed-plain | wait | failed | 1 | yes | yes | none | yes | 797 ms | pass |
| claude-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 830 ms | pass |
| claude-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 163 ms | pass |
| claude-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 148 ms | pass |
| claude-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 155 ms | pass |
| claude-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1651 ms | pass |
| claude-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1714 ms | pass |
| claude-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 170 ms | pass |
| claude-fd-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 154 ms | expected_failure |
| claude-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 159 ms | pass |
| claude-fd-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 160 ms | expected_failure |
| claude-fd-bare | wait | timed_out | 124 | yes | yes | none | yes | 146 ms | pass |
| claude-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 161 ms | pass |
| claude-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 146 ms | pass |
| claude-i-plain | wait | failed | 1 | yes | yes | none | yes | 211 ms | pass |
| claude-i-child | wait | failed | 1 | yes | yes | none | yes | 331 ms | pass |
| claude-i-setsid | wait | failed | 1 | yes | yes | none | yes | 353 ms | pass |
| claude-id-plain | wait | failed | 1 | yes | yes | none | yes | 221 ms | pass |
| claude-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 255 ms | pass |
| claude-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 294 ms | pass |
| claude-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1776 ms | pass |
| claude-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 179 ms | pass |
| claude-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 231 ms | pass |
| claude-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 141 ms | pass |
| claude-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 166 ms | pass |
| claude-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 143 ms | pass |
| claude-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1772 ms | pass |
| claude-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| claude-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| claude-k-ignore | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-pipe | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-orphan | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | leaf-setsid | n/a | 6 ms | expected_failure |
| claude-k-orphan-late | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-relay | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | relay-heartbeat | n/a | 6 ms | expected_failure |
| claude-k-bare | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| claude-kk-plain | SIGKILL to delega and its keeper at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 7 ms | expected_failure |
| claude-u-plain | wait | completed | 0 | yes | NO | none | yes | 64 ms | pass |
| claude-u-child | wait | completed | 0 | yes | NO | none | yes | 119 ms | pass |
| claude-u-setsid | wait | completed | 0 | yes | NO | leaf-setsid | yes | 120 ms | pass |
| claude-u-orphan | wait | completed | 0 | yes | NO | leaf-setsid | yes | 428 ms | pass |
| claude-u-bare | wait | completed | 0 | yes | NO | bare | yes | 439 ms | pass |
| claude-ud-plain | wait | timed_out | 124 | yes | NO | none | yes | 40 ms | pass |
| claude-ud-child | wait | timed_out | 124 | yes | NO | none | yes | 35 ms | pass |
| claude-ud-setsid | wait | timed_out | 124 | yes | NO | leaf-setsid | yes | 31 ms | pass |
| claude-ud-stubborn | wait | timed_out | 124 | yes | NO | none | yes | 1526 ms | pass |
| claude-ui-plain | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 34 ms | pass |
| claude-ui-child | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 32 ms | pass |
| claude-ui-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | NO | leaf-setsid | yes | 37 ms | pass |
| claude-ui-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 32 ms | pass |
| codex-a-plain | wait | completed | 0 | yes | yes | none | yes | 301 ms | pass |
| codex-a-child | wait | completed | 0 | yes | yes | none | yes | 419 ms | pass |
| codex-a-setsid | wait | completed | 0 | yes | yes | none | yes | 444 ms | pass |
| codex-a-ignore | wait | completed | 0 | yes | yes | none | yes | 1980 ms | pass |
| codex-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 311 ms | pass |
| codex-a-pipe | wait | completed | 0 | yes | yes | none | yes | 436 ms | pass |
| codex-a-orphan | wait | completed | 0 | yes | yes | leaf-setsid | NO | 772 ms | expected_failure |
| codex-a-orphan-late | wait | completed | 0 | yes | yes | none | yes | 439 ms | pass |
| codex-a-relay | wait | completed | 0 | yes | yes | relay-heartbeat | NO | 460 ms | expected_failure |
| codex-a-bare | wait | completed | 0 | yes | yes | bare | NO | 3539 ms | expected_failure |
| codex-b-plain | wait | failed | 1 | yes | yes | none | yes | 306 ms | pass |
| codex-b-child | wait | failed | 1 | yes | yes | none | yes | 416 ms | pass |
| codex-b-setsid | wait | failed | 1 | yes | yes | none | yes | 456 ms | pass |
| codex-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 148 ms | pass |
| codex-c-child | wait | timed_out | 124 | yes | yes | none | yes | 143 ms | pass |
| codex-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 142 ms | pass |
| codex-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1670 ms | pass |
| codex-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1695 ms | pass |
| codex-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 166 ms | pass |
| codex-c-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 167 ms | expected_failure |
| codex-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 141 ms | pass |
| codex-c-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 168 ms | expected_failure |
| codex-c-bare | wait | timed_out | 124 | yes | yes | none | yes | 158 ms | pass |
| codex-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 133 ms | pass |
| codex-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 138 ms | pass |
| codex-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 152 ms | pass |
| codex-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1685 ms | pass |
| codex-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1694 ms | pass |
| codex-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 177 ms | pass |
| codex-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | leaf-setsid | NO | 190 ms | expected_failure |
| codex-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 155 ms | pass |
| codex-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | relay-heartbeat | NO | 160 ms | expected_failure |
| codex-d-bare | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 138 ms | pass |
| codex-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 163 ms | pass |
| codex-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 166 ms | pass |
| codex-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 141 ms | pass |
| codex-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1700 ms | pass |
| codex-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 140 ms | pass |
| codex-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 137 ms | pass |
| codex-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 133 ms | pass |
| codex-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1702 ms | pass |
| codex-e-plain | wait | failed | 1 | yes | yes | none | yes | 843 ms | pass |
| codex-e-child | wait | failed | 1 | yes | yes | none | yes | 939 ms | pass |
| codex-e-setsid | wait | failed | 1 | yes | yes | none | yes | 935 ms | pass |
| codex-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2410 ms | pass |
| codex-e-pipe | wait | failed | 1 | yes | yes | none | yes | 921 ms | pass |
| codex-e-orphan | wait | failed | 1 | yes | yes | leaf-setsid | NO | 1230 ms | expected_failure |
| codex-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 919 ms | pass |
| codex-e-relay | wait | failed | 1 | yes | yes | relay-heartbeat | NO | 922 ms | expected_failure |
| codex-e-bare | wait | failed | 1 | yes | yes | none | yes | 1944 ms | pass |
| codex-ed-plain | wait | failed | 1 | yes | yes | none | yes | 885 ms | pass |
| codex-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 912 ms | pass |
| codex-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 163 ms | pass |
| codex-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 165 ms | pass |
| codex-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 147 ms | pass |
| codex-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1626 ms | pass |
| codex-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1708 ms | pass |
| codex-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 161 ms | pass |
| codex-fd-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 148 ms | expected_failure |
| codex-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 147 ms | pass |
| codex-fd-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 157 ms | expected_failure |
| codex-fd-bare | wait | timed_out | 124 | yes | yes | none | yes | 141 ms | pass |
| codex-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 155 ms | pass |
| codex-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 172 ms | pass |
| codex-i-plain | wait | failed | 1 | yes | yes | none | yes | 298 ms | pass |
| codex-i-child | wait | failed | 1 | yes | yes | none | yes | 440 ms | pass |
| codex-i-setsid | wait | failed | 1 | yes | yes | none | yes | 433 ms | pass |
| codex-id-plain | wait | failed | 1 | yes | yes | none | yes | 313 ms | pass |
| codex-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 209 ms | pass |
| codex-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 259 ms | pass |
| codex-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1749 ms | pass |
| codex-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 183 ms | pass |
| codex-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 234 ms | pass |
| codex-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 166 ms | pass |
| codex-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 143 ms | pass |
| codex-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 167 ms | pass |
| codex-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1861 ms | pass |
| codex-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| codex-k-ignore | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-pipe | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-orphan | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | leaf-setsid | n/a | 7 ms | expected_failure |
| codex-k-orphan-late | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-relay | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | relay-heartbeat | n/a | 7 ms | expected_failure |
| codex-k-bare | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-kk-plain | SIGKILL to delega and its keeper at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 7 ms | expected_failure |
| codex-u-plain | wait | completed | 0 | yes | NO | none | yes | 120 ms | pass |
| codex-u-child | wait | completed | 0 | yes | NO | none | yes | 223 ms | pass |
| codex-u-setsid | wait | completed | 0 | yes | NO | leaf-setsid | yes | 202 ms | pass |
| codex-u-orphan | wait | completed | 0 | yes | NO | leaf-setsid | yes | 507 ms | pass |
| codex-u-bare | wait | completed | 0 | yes | NO | bare | yes | 459 ms | pass |
| codex-ud-plain | wait | timed_out | 124 | yes | NO | none | yes | 19 ms | pass |
| codex-ud-child | wait | timed_out | 124 | yes | NO | none | yes | 15 ms | pass |
| codex-ud-setsid | wait | timed_out | 124 | yes | NO | leaf-setsid | yes | 16 ms | pass |
| codex-ud-stubborn | wait | timed_out | 124 | yes | NO | none | yes | 1518 ms | pass |
| codex-ui-plain | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 33 ms | pass |
| codex-ui-child | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 34 ms | pass |
| codex-ui-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | NO | leaf-setsid | yes | 37 ms | pass |
| codex-ui-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 24 ms | pass |

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

# Results: does delega meet criteria 1 and 2?

Generated from `results.jsonl` by `cargo run -p delega-acceptance -- --report`; do not edit by hand.
The harness is described in [README.md](README.md). Criteria, from the repository README:
(1) every run ends in exactly one stated outcome, never a hang and never silence; (2) after any
outcome the provider and its descendants are gone, or the result says cleanup could not be
confirmed and what remains.

## Environment

- Machine: Darwin 27.2.0 arm64.
- The system hides the environment of its own programs from `ps -E`: yes. On macOS the orphan rows are expected failures when it does and passes when it does not (process module docs, "What remains possible").
- delega: version 0.1.0-dev.
- Harness commit: `0608770`.
- 202 rows, 8 at a time, finished in 95 s.
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
| all | 202 | 202 | 202 | 172 of 172 | 200 | 172 of 172 | 200 | 2 | 0 | 0 | 0 |
| claude | 101 | 101 | 101 | 86 of 86 | 100 | 86 of 86 | 100 | 1 | 0 | 0 | 0 |
| codex | 101 | 101 | 101 | 86 of 86 | 100 | 86 of 86 | 100 | 1 | 0 | 0 | 0 |

### By caller action

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Honest report | Pass | Expected failure | Gap not shown | Unexpected pass | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| provider ends by itself | 56 | 56 | 56 | 56 of 56 | 56 | 56 of 56 | 56 | 0 | 0 | 0 | 0 |
| time limit (--timeout) | 50 | 50 | 50 | 50 of 50 | 50 | 50 of 50 | 50 | 0 | 0 | 0 | 0 |
| SIGINT | 24 | 24 | 24 | 24 of 24 | 24 | 24 of 24 | 24 | 0 | 0 | 0 | 0 |
| SIGTERM | 8 | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 0 | 0 | 0 | 0 |
| SIGHUP | 8 | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 0 | 0 | 0 | 0 |
| stdout closed early | 8 | 8 | 8 | 0 of 0 | 8 | 0 of 0 | 8 | 0 | 0 | 0 | 0 |
| SIGKILL of delega | 20 | 20 | 20 | 0 of 0 | 20 | 0 of 0 | 20 | 0 | 0 | 0 | 0 |
| SIGKILL of delega and its keeper | 2 | 2 | 2 | 0 of 0 | 0 | 0 of 0 | 0 | 2 | 0 | 0 | 0 |
| other | 26 | 26 | 26 | 26 of 26 | 26 | 26 of 26 | 26 | 0 | 0 | 0 | 0 |

### By variant (what the provider leaves running)

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Honest report | Pass | Expected failure | Gap not shown | Unexpected pass | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| plain | 42 | 42 | 42 | 34 of 34 | 40 | 34 of 34 | 40 | 2 | 0 | 0 | 0 |
| child | 34 | 34 | 34 | 30 of 30 | 34 | 30 of 30 | 34 | 0 | 0 | 0 | 0 |
| pipe | 14 | 14 | 14 | 12 of 12 | 14 | 12 of 12 | 14 | 0 | 0 | 0 | 0 |
| ignore | 16 | 16 | 16 | 14 of 14 | 16 | 14 of 14 | 16 | 0 | 0 | 0 | 0 |
| stubborn | 16 | 16 | 16 | 14 of 14 | 16 | 14 of 14 | 16 | 0 | 0 | 0 | 0 |
| setsid | 28 | 28 | 28 | 24 of 24 | 28 | 24 of 24 | 28 | 0 | 0 | 0 | 0 |
| orphan | 14 | 14 | 14 | 12 of 12 | 14 | 12 of 12 | 14 | 0 | 0 | 0 | 0 |
| orphan-late | 12 | 12 | 12 | 10 of 10 | 12 | 10 of 10 | 12 | 0 | 0 | 0 | 0 |
| relay | 12 | 12 | 12 | 10 of 10 | 12 | 10 of 10 | 12 | 0 | 0 | 0 | 0 |

## Latency

Measured from the harness's action to `delega` exiting (a signal), past the `--timeout` (a time
limit that fired), or from start to exit (a run left to finish). A process that ignores TERM adds the
grace period before SIGKILL. SIGKILL rows are left out.

| Measure | Rows | p50 | p90 | p99 | max |
| --- | --- | --- | --- | --- | --- |
| signal to exit | 40 | 180 ms | 288 ms | 1716 ms | 1716 ms |
| signal to exit, a process ignores TERM | 10 | 1713 ms | 1746 ms | 1764 ms | 1764 ms |
| timeout overshoot | 52 | 189 ms | 1690 ms | 1725 ms | 1725 ms |
| timeout overshoot, a process ignores TERM | 12 | 1710 ms | 1787 ms | 1794 ms | 1794 ms |
| run to exit | 62 | 807 ms | 2405 ms | 4915 ms | 4915 ms |
| run to exit, a process ignores TERM | 6 | 2020 ms | 2501 ms | 2501 ms | 2501 ms |

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

- documented gap: 2 rows.
- undocumented gap: 0 rows.
- defect: 0 rows.

### documented gap: 2 rows, 2 failed as expected in this run

SIGKILL of delega and of its keeper together: no code of either runs, so nothing stops the worker or enforces its time limit. The accepted residual on every platform (process module docs, "What remains possible").

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest |
| --- | --- | --- | --- | --- | --- |
| claude-kk-plain | none | signal 9 | n/a | provider | n/a |
| codex-kk-plain | none | signal 9 | n/a | provider | n/a |

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
| claude-u-setsid | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-u-orphan | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-u-bare | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ud-plain | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ud-child | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ud-setsid | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ud-stubborn | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ui-plain | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ui-child | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ui-setsid | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| claude-ui-ignore | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-u-plain | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-u-child | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-u-setsid | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-u-orphan | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-u-bare | completed | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ud-plain | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ud-child | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ud-setsid | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ud-stubborn | timed_out | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ui-plain | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ui-child | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
| codex-ui-setsid | interrupted | NO | [] | [] | none | process inspection was unavailable (ps could not be started (no ps pro |
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
| 1 | 200 | 2 | 0 | 0 | 0 | 4524 ms | 4810 ms |
| 2 | 200 | 2 | 0 | 0 | 0 | 3658 ms | 4954 ms |
| 3 | 200 | 2 | 0 | 0 | 0 | 4288 ms | 5068 ms |

No row changed verdict, outcome, exit status, survivors or cleanup report between runs.

## All rows

| Row | Action | Outcome | Exit | One document | Cleanup confirmed | Left running | Honest | Latency | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| claude-a-plain | wait | completed | 0 | yes | yes | none | yes | 695 ms | pass |
| claude-a-child | wait | completed | 0 | yes | yes | none | yes | 815 ms | pass |
| claude-a-setsid | wait | completed | 0 | yes | yes | none | yes | 845 ms | pass |
| claude-a-ignore | wait | completed | 0 | yes | yes | none | yes | 2352 ms | pass |
| claude-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 707 ms | pass |
| claude-a-pipe | wait | completed | 0 | yes | yes | none | yes | 807 ms | pass |
| claude-a-orphan | wait | completed | 0 | yes | yes | none | yes | 2668 ms | pass |
| claude-a-orphan-late | wait | completed | 0 | yes | yes | none | yes | 810 ms | pass |
| claude-a-relay | wait | completed | 0 | yes | yes | none | yes | 1926 ms | pass |
| claude-a-bare | wait | completed | 0 | yes | yes | none | yes | 4915 ms | pass |
| claude-b-plain | wait | failed | 1 | yes | yes | none | yes | 272 ms | pass |
| claude-b-child | wait | failed | 1 | yes | yes | none | yes | 426 ms | pass |
| claude-b-setsid | wait | failed | 1 | yes | yes | none | yes | 400 ms | pass |
| claude-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 210 ms | pass |
| claude-c-child | wait | timed_out | 124 | yes | yes | none | yes | 168 ms | pass |
| claude-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 184 ms | pass |
| claude-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1697 ms | pass |
| claude-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1740 ms | pass |
| claude-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 191 ms | pass |
| claude-c-orphan | wait | timed_out | 124 | yes | yes | none | yes | 1707 ms | pass |
| claude-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 189 ms | pass |
| claude-c-relay | wait | timed_out | 124 | yes | yes | none | yes | 1687 ms | pass |
| claude-c-bare | wait | timed_out | 124 | yes | yes | none | yes | 185 ms | pass |
| claude-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 170 ms | pass |
| claude-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 192 ms | pass |
| claude-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 181 ms | pass |
| claude-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1688 ms | pass |
| claude-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1724 ms | pass |
| claude-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 168 ms | pass |
| claude-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1673 ms | pass |
| claude-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 178 ms | pass |
| claude-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1681 ms | pass |
| claude-d-bare | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 173 ms | pass |
| claude-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 198 ms | pass |
| claude-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 170 ms | pass |
| claude-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 166 ms | pass |
| claude-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1713 ms | pass |
| claude-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 180 ms | pass |
| claude-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 166 ms | pass |
| claude-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 166 ms | pass |
| claude-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1746 ms | pass |
| claude-e-plain | wait | failed | 1 | yes | yes | none | yes | 853 ms | pass |
| claude-e-child | wait | failed | 1 | yes | yes | none | yes | 894 ms | pass |
| claude-e-setsid | wait | failed | 1 | yes | yes | none | yes | 873 ms | pass |
| claude-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2400 ms | pass |
| claude-e-pipe | wait | failed | 1 | yes | yes | none | yes | 906 ms | pass |
| claude-e-orphan | wait | failed | 1 | yes | yes | none | yes | 2751 ms | pass |
| claude-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 905 ms | pass |
| claude-e-relay | wait | failed | 1 | yes | yes | none | yes | 2405 ms | pass |
| claude-e-bare | wait | failed | 1 | yes | yes | none | yes | 1910 ms | pass |
| claude-ed-plain | wait | failed | 1 | yes | yes | none | yes | 861 ms | pass |
| claude-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 920 ms | pass |
| claude-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 175 ms | pass |
| claude-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 181 ms | pass |
| claude-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 192 ms | pass |
| claude-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1706 ms | pass |
| claude-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1731 ms | pass |
| claude-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 200 ms | pass |
| claude-fd-orphan | wait | timed_out | 124 | yes | yes | none | yes | 1725 ms | pass |
| claude-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 189 ms | pass |
| claude-fd-relay | wait | timed_out | 124 | yes | yes | none | yes | 1719 ms | pass |
| claude-fd-bare | wait | timed_out | 124 | yes | yes | none | yes | 189 ms | pass |
| claude-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 213 ms | pass |
| claude-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 206 ms | pass |
| claude-i-plain | wait | failed | 1 | yes | yes | none | yes | 230 ms | pass |
| claude-i-child | wait | failed | 1 | yes | yes | none | yes | 387 ms | pass |
| claude-i-setsid | wait | failed | 1 | yes | yes | none | yes | 396 ms | pass |
| claude-id-plain | wait | failed | 1 | yes | yes | none | yes | 267 ms | pass |
| claude-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 287 ms | pass |
| claude-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 356 ms | pass |
| claude-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1794 ms | pass |
| claude-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 220 ms | pass |
| claude-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 288 ms | pass |
| claude-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 187 ms | pass |
| claude-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 172 ms | pass |
| claude-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 187 ms | pass |
| claude-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1844 ms | pass |
| claude-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-ignore | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| claude-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-pipe | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-orphan | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-orphan-late | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-k-relay | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| claude-k-bare | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| claude-kk-plain | SIGKILL to delega and its keeper at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 5 ms | expected_failure |
| claude-u-plain | wait | completed | 0 | yes | NO | none | yes | 108 ms | pass |
| claude-u-child | wait | completed | 0 | yes | NO | none | yes | 167 ms | pass |
| claude-u-setsid | wait | completed | 0 | yes | NO | none | yes | 168 ms | pass |
| claude-u-orphan | wait | completed | 0 | yes | NO | none | yes | 511 ms | pass |
| claude-u-bare | wait | completed | 0 | yes | NO | none | yes | 489 ms | pass |
| claude-ud-plain | wait | timed_out | 124 | yes | NO | none | yes | 70 ms | pass |
| claude-ud-child | wait | timed_out | 124 | yes | NO | none | yes | 53 ms | pass |
| claude-ud-setsid | wait | timed_out | 124 | yes | NO | none | yes | 54 ms | pass |
| claude-ud-stubborn | wait | timed_out | 124 | yes | NO | none | yes | 1580 ms | pass |
| claude-ui-plain | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 55 ms | pass |
| claude-ui-child | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 55 ms | pass |
| claude-ui-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 54 ms | pass |
| claude-ui-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 35 ms | pass |
| codex-a-plain | wait | completed | 0 | yes | yes | none | yes | 382 ms | pass |
| codex-a-child | wait | completed | 0 | yes | yes | none | yes | 509 ms | pass |
| codex-a-setsid | wait | completed | 0 | yes | yes | none | yes | 537 ms | pass |
| codex-a-ignore | wait | completed | 0 | yes | yes | none | yes | 2020 ms | pass |
| codex-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 393 ms | pass |
| codex-a-pipe | wait | completed | 0 | yes | yes | none | yes | 525 ms | pass |
| codex-a-orphan | wait | completed | 0 | yes | yes | none | yes | 2354 ms | pass |
| codex-a-orphan-late | wait | completed | 0 | yes | yes | none | yes | 523 ms | pass |
| codex-a-relay | wait | completed | 0 | yes | yes | none | yes | 2003 ms | pass |
| codex-a-bare | wait | completed | 0 | yes | yes | none | yes | 4640 ms | pass |
| codex-b-plain | wait | failed | 1 | yes | yes | none | yes | 381 ms | pass |
| codex-b-child | wait | failed | 1 | yes | yes | none | yes | 529 ms | pass |
| codex-b-setsid | wait | failed | 1 | yes | yes | none | yes | 512 ms | pass |
| codex-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 184 ms | pass |
| codex-c-child | wait | timed_out | 124 | yes | yes | none | yes | 226 ms | pass |
| codex-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 186 ms | pass |
| codex-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1710 ms | pass |
| codex-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1775 ms | pass |
| codex-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 189 ms | pass |
| codex-c-orphan | wait | timed_out | 124 | yes | yes | none | yes | 1682 ms | pass |
| codex-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 209 ms | pass |
| codex-c-relay | wait | timed_out | 124 | yes | yes | none | yes | 1692 ms | pass |
| codex-c-bare | wait | timed_out | 124 | yes | yes | none | yes | 186 ms | pass |
| codex-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 199 ms | pass |
| codex-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 202 ms | pass |
| codex-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 185 ms | pass |
| codex-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1646 ms | pass |
| codex-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1764 ms | pass |
| codex-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 180 ms | pass |
| codex-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1716 ms | pass |
| codex-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 180 ms | pass |
| codex-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1655 ms | pass |
| codex-d-bare | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 187 ms | pass |
| codex-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 190 ms | pass |
| codex-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 201 ms | pass |
| codex-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 199 ms | pass |
| codex-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1728 ms | pass |
| codex-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 194 ms | pass |
| codex-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 163 ms | pass |
| codex-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 180 ms | pass |
| codex-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1722 ms | pass |
| codex-e-plain | wait | failed | 1 | yes | yes | none | yes | 947 ms | pass |
| codex-e-child | wait | failed | 1 | yes | yes | none | yes | 1016 ms | pass |
| codex-e-setsid | wait | failed | 1 | yes | yes | none | yes | 1030 ms | pass |
| codex-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2501 ms | pass |
| codex-e-pipe | wait | failed | 1 | yes | yes | none | yes | 1018 ms | pass |
| codex-e-orphan | wait | failed | 1 | yes | yes | none | yes | 2833 ms | pass |
| codex-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 1062 ms | pass |
| codex-e-relay | wait | failed | 1 | yes | yes | none | yes | 2515 ms | pass |
| codex-e-bare | wait | failed | 1 | yes | yes | none | yes | 2050 ms | pass |
| codex-ed-plain | wait | failed | 1 | yes | yes | none | yes | 979 ms | pass |
| codex-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 1029 ms | pass |
| codex-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 169 ms | pass |
| codex-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 197 ms | pass |
| codex-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 175 ms | pass |
| codex-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1659 ms | pass |
| codex-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1728 ms | pass |
| codex-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 188 ms | pass |
| codex-fd-orphan | wait | timed_out | 124 | yes | yes | none | yes | 1690 ms | pass |
| codex-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 174 ms | pass |
| codex-fd-relay | wait | timed_out | 124 | yes | yes | none | yes | 1695 ms | pass |
| codex-fd-bare | wait | timed_out | 124 | yes | yes | none | yes | 189 ms | pass |
| codex-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 171 ms | pass |
| codex-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 199 ms | pass |
| codex-i-plain | wait | failed | 1 | yes | yes | none | yes | 392 ms | pass |
| codex-i-child | wait | failed | 1 | yes | yes | none | yes | 502 ms | pass |
| codex-i-setsid | wait | failed | 1 | yes | yes | none | yes | 535 ms | pass |
| codex-id-plain | wait | failed | 1 | yes | yes | none | yes | 391 ms | pass |
| codex-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 239 ms | pass |
| codex-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 292 ms | pass |
| codex-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1787 ms | pass |
| codex-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 196 ms | pass |
| codex-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 266 ms | pass |
| codex-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 218 ms | pass |
| codex-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 186 ms | pass |
| codex-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 191 ms | pass |
| codex-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1954 ms | pass |
| codex-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| codex-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-ignore | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| codex-k-pipe | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 7 ms | pass |
| codex-k-orphan | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 5 ms | pass |
| codex-k-orphan-late | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-relay | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-k-bare | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | none | n/a | 6 ms | pass |
| codex-kk-plain | SIGKILL to delega and its keeper at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 6 ms | expected_failure |
| codex-u-plain | wait | completed | 0 | yes | NO | none | yes | 162 ms | pass |
| codex-u-child | wait | completed | 0 | yes | NO | none | yes | 205 ms | pass |
| codex-u-setsid | wait | completed | 0 | yes | NO | none | yes | 232 ms | pass |
| codex-u-orphan | wait | completed | 0 | yes | NO | none | yes | 527 ms | pass |
| codex-u-bare | wait | completed | 0 | yes | NO | none | yes | 520 ms | pass |
| codex-ud-plain | wait | timed_out | 124 | yes | NO | none | yes | 54 ms | pass |
| codex-ud-child | wait | timed_out | 124 | yes | NO | none | yes | 62 ms | pass |
| codex-ud-setsid | wait | timed_out | 124 | yes | NO | none | yes | 62 ms | pass |
| codex-ud-stubborn | wait | timed_out | 124 | yes | NO | none | yes | 1578 ms | pass |
| codex-ui-plain | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 60 ms | pass |
| codex-ui-child | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 60 ms | pass |
| codex-ui-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 59 ms | pass |
| codex-ui-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 28 ms | pass |

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

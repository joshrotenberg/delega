# Results: does delega meet criteria 1 and 2?

Generated from `results.jsonl` by `cargo run -p delega-acceptance -- --report`; do not edit by hand.
The harness is described in [README.md](README.md). Criteria, from the repository README:
(1) every run ends in exactly one stated outcome, never a hang and never silence; (2) after any
outcome the provider and its descendants are gone, or the result says cleanup could not be
confirmed and what remains.

## Environment

- Machine: Darwin 27.2.0 arm64.
- delega: version 0.1.0-dev.
- Harness commit: `2ddaca5`.
- 188 rows, 8 at a time, finished in 82 s.
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
| all | 188 | 188 | 188 | 172 of 172 | 149 | 151 of 172 | 158 | 29 | 1 | 0 | 0 |
| claude | 94 | 94 | 94 | 86 of 86 | 75 | 76 of 86 | 79 | 14 | 1 | 0 | 0 |
| codex | 94 | 94 | 94 | 86 of 86 | 74 | 75 of 86 | 79 | 15 | 0 | 0 | 0 |

### By caller action

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Honest report | Pass | Expected failure | Gap not shown | Unexpected pass | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| provider ends by itself | 56 | 56 | 56 | 56 of 56 | 47 | 47 of 56 | 46 | 9 | 1 | 0 | 0 |
| time limit (--timeout) | 50 | 50 | 50 | 50 of 50 | 42 | 42 of 50 | 42 | 8 | 0 | 0 | 0 |
| SIGINT | 24 | 24 | 24 | 24 of 24 | 20 | 20 of 24 | 20 | 4 | 0 | 0 | 0 |
| SIGTERM | 8 | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 0 | 0 | 0 | 0 |
| SIGHUP | 8 | 8 | 8 | 8 of 8 | 8 | 8 of 8 | 8 | 0 | 0 | 0 | 0 |
| stdout closed early | 8 | 8 | 8 | 0 of 0 | 8 | 0 of 0 | 8 | 0 | 0 | 0 | 0 |
| SIGKILL of delega | 8 | 8 | 8 | 0 of 0 | 0 | 0 of 0 | 0 | 8 | 0 | 0 | 0 |
| other | 26 | 26 | 26 | 26 of 26 | 16 | 26 of 26 | 26 | 0 | 0 | 0 | 0 |

### By variant (what the provider leaves running)

| Group | Rows | Expected outcome and exit | Cleanup verdict as expected | One stated outcome | Nothing left | Honest report | Pass | Expected failure | Gap not shown | Unexpected pass | Fail |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| plain | 40 | 40 | 40 | 34 of 34 | 38 | 34 of 34 | 38 | 2 | 0 | 0 | 0 |
| child | 34 | 34 | 34 | 30 of 30 | 32 | 30 of 30 | 32 | 2 | 0 | 0 | 0 |
| pipe | 12 | 12 | 12 | 12 of 12 | 12 | 12 of 12 | 12 | 0 | 0 | 0 | 0 |
| ignore | 14 | 14 | 14 | 14 of 14 | 14 | 14 of 14 | 14 | 0 | 0 | 0 | 0 |
| stubborn | 16 | 16 | 16 | 14 of 14 | 14 | 14 of 14 | 14 | 2 | 0 | 0 | 0 |
| setsid | 28 | 28 | 28 | 24 of 24 | 20 | 24 of 24 | 26 | 2 | 0 | 0 | 0 |
| orphan | 12 | 12 | 12 | 12 of 12 | 1 | 3 of 12 | 2 | 9 | 1 | 0 | 0 |
| orphan-late | 10 | 10 | 10 | 10 of 10 | 10 | 10 of 10 | 10 | 0 | 0 | 0 | 0 |
| relay | 10 | 10 | 10 | 10 of 10 | 0 | 0 of 10 | 0 | 10 | 0 | 0 | 0 |

## Latency

Measured from the harness's action to `delega` exiting (a signal), past the `--timeout` (a time
limit that fired), or from start to exit (a run left to finish). A process that ignores TERM adds the
grace period before SIGKILL. SIGKILL rows are left out.

| Measure | Rows | p50 | p90 | p99 | max |
| --- | --- | --- | --- | --- | --- |
| signal to exit | 38 | 93 ms | 181 ms | 251 ms | 251 ms |
| signal to exit, a process ignores TERM | 10 | 1600 ms | 1628 ms | 1630 ms | 1630 ms |
| timeout overshoot | 52 | 104 ms | 134 ms | 424 ms | 424 ms |
| timeout overshoot, a process ignores TERM | 12 | 1626 ms | 1763 ms | 1780 ms | 1780 ms |
| run to exit | 62 | 421 ms | 808 ms | 1803 ms | 1803 ms |
| run to exit, a process ignores TERM | 6 | 1751 ms | 2298 ms | 2298 ms | 2298 ms |

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

- documented gap: 30 rows.
- undocumented gap: 0 rows.
- defect: 0 rows.

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

### documented gap: 10 rows, 9 failed as expected in this run

The descendant called setsid and its parent, a child of the provider, exited at once, so the descendant was a child of init in a session of its own before any listing could see it with a parent that was ours: the listing at spawn, the early ones at 50, 100 and 200 ms, or the periodic one. The rows are timing-dependent: a listing that lands between the descendant's start and its parent's exit adopts the descendant, and the gap does not show. No process table link ties it to the worker, so cleanup reports confirmed while it runs (process module gap 1).

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest |
| --- | --- | --- | --- | --- | --- |
| claude-a-orphan | completed | 0 | yes | none | yes |
| claude-c-orphan | timed_out | 124 | yes | leaf-setsid | NO |
| claude-d-orphan | interrupted | 130 | yes | leaf-setsid | NO |
| claude-e-orphan | failed | 1 | yes | leaf-setsid | NO |
| claude-fd-orphan | timed_out | 124 | yes | leaf-setsid | NO |
| codex-a-orphan | completed | 0 | yes | leaf-setsid | NO |
| codex-c-orphan | timed_out | 124 | yes | leaf-setsid | NO |
| codex-d-orphan | interrupted | 130 | yes | leaf-setsid | NO |
| codex-e-orphan | failed | 1 | yes | leaf-setsid | NO |
| codex-fd-orphan | timed_out | 124 | yes | leaf-setsid | NO |

### documented gap: 2 rows, 2 failed as expected in this run

The process is a direct child of the provider that called setsid and the provider ended by itself between two listings, which are 50 to 100 ms apart in the first 200 ms of a run and 500 ms apart after it. When the provider exits, its children become children of init at once, so no listing taken afterwards ties the process to the worker, and no scan ran while the provider was its parent. Cleanup reports confirmed while it runs (process module gap 1).

| Row | Outcome | Exit | Cleanup confirmed | Left running | Report honest |
| --- | --- | --- | --- | --- | --- |
| claude-a-bare | completed | 0 | yes | bare | NO |
| codex-a-bare | completed | 0 | yes | bare | NO |

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

`repeats.jsonl` holds 3 full runs of the whole matrix (188 rows each).

| Run | Pass | Expected failure | Gap not shown | Unexpected pass | Fail | p99 latency (not SIGKILL) | Max latency |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 158 | 30 | 0 | 0 | 0 | 2242 ms | 2317 ms |
| 2 | 158 | 30 | 0 | 0 | 0 | 2254 ms | 2329 ms |
| 3 | 158 | 30 | 0 | 0 | 0 | 2259 ms | 2335 ms |

No row changed verdict, outcome, exit status, survivors or cleanup report between runs.

## All rows

| Row | Action | Outcome | Exit | One document | Cleanup confirmed | Left running | Honest | Latency | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| claude-a-plain | wait | completed | 0 | yes | yes | none | yes | 574 ms | pass |
| claude-a-child | wait | completed | 0 | yes | yes | none | yes | 711 ms | pass |
| claude-a-setsid | wait | completed | 0 | yes | yes | none | yes | 673 ms | pass |
| claude-a-ignore | wait | completed | 0 | yes | yes | none | yes | 2162 ms | pass |
| claude-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 579 ms | pass |
| claude-a-pipe | wait | completed | 0 | yes | yes | none | yes | 672 ms | pass |
| claude-a-orphan | wait | completed | 0 | yes | yes | none | yes | 691 ms | gap_not_shown |
| claude-a-orphan-late | wait | completed | 0 | yes | yes | none | yes | 711 ms | pass |
| claude-a-relay | wait | completed | 0 | yes | yes | relay-heartbeat | NO | 244 ms | expected_failure |
| claude-a-bare | wait | completed | 0 | yes | yes | bare | NO | 538 ms | expected_failure |
| claude-b-plain | wait | failed | 1 | yes | yes | none | yes | 154 ms | pass |
| claude-b-child | wait | failed | 1 | yes | yes | none | yes | 234 ms | pass |
| claude-b-setsid | wait | failed | 1 | yes | yes | none | yes | 248 ms | pass |
| claude-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 105 ms | pass |
| claude-c-child | wait | timed_out | 124 | yes | yes | none | yes | 104 ms | pass |
| claude-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 105 ms | pass |
| claude-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1634 ms | pass |
| claude-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1626 ms | pass |
| claude-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 114 ms | pass |
| claude-c-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 86 ms | expected_failure |
| claude-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 114 ms | pass |
| claude-c-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 124 ms | expected_failure |
| claude-c-bare | wait | timed_out | 124 | yes | yes | none | yes | 106 ms | pass |
| claude-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 111 ms | pass |
| claude-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 91 ms | pass |
| claude-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 88 ms | pass |
| claude-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1614 ms | pass |
| claude-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1628 ms | pass |
| claude-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 106 ms | pass |
| claude-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | leaf-setsid | NO | 88 ms | expected_failure |
| claude-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 93 ms | pass |
| claude-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | relay-heartbeat | NO | 88 ms | expected_failure |
| claude-d-bare | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 105 ms | pass |
| claude-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 92 ms | pass |
| claude-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 112 ms | pass |
| claude-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 116 ms | pass |
| claude-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1600 ms | pass |
| claude-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 89 ms | pass |
| claude-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 98 ms | pass |
| claude-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 89 ms | pass |
| claude-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1621 ms | pass |
| claude-e-plain | wait | failed | 1 | yes | yes | none | yes | 665 ms | pass |
| claude-e-child | wait | failed | 1 | yes | yes | none | yes | 731 ms | pass |
| claude-e-setsid | wait | failed | 1 | yes | yes | none | yes | 766 ms | pass |
| claude-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2274 ms | pass |
| claude-e-pipe | wait | failed | 1 | yes | yes | none | yes | 743 ms | pass |
| claude-e-orphan | wait | failed | 1 | yes | yes | leaf-setsid | NO | 759 ms | expected_failure |
| claude-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 745 ms | pass |
| claude-e-relay | wait | failed | 1 | yes | yes | relay-heartbeat | NO | 739 ms | expected_failure |
| claude-e-bare | wait | failed | 1 | yes | yes | none | yes | 1758 ms | pass |
| claude-ed-plain | wait | failed | 1 | yes | yes | none | yes | 696 ms | pass |
| claude-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 751 ms | pass |
| claude-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 90 ms | pass |
| claude-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 103 ms | pass |
| claude-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 108 ms | pass |
| claude-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1612 ms | pass |
| claude-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1619 ms | pass |
| claude-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 109 ms | pass |
| claude-fd-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 102 ms | expected_failure |
| claude-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 89 ms | pass |
| claude-fd-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 112 ms | expected_failure |
| claude-fd-bare | wait | timed_out | 124 | yes | yes | none | yes | 90 ms | pass |
| claude-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 86 ms | pass |
| claude-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 126 ms | pass |
| claude-i-plain | wait | failed | 1 | yes | yes | none | yes | 122 ms | pass |
| claude-i-child | wait | failed | 1 | yes | yes | none | yes | 224 ms | pass |
| claude-i-setsid | wait | failed | 1 | yes | yes | none | yes | 221 ms | pass |
| claude-id-plain | wait | failed | 1 | yes | yes | none | yes | 113 ms | pass |
| claude-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 383 ms | pass |
| claude-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 424 ms | pass |
| claude-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1780 ms | pass |
| claude-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 204 ms | pass |
| claude-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 251 ms | pass |
| claude-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 138 ms | pass |
| claude-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 110 ms | pass |
| claude-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 109 ms | pass |
| claude-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1743 ms | pass |
| claude-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 7 ms | expected_failure |
| claude-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | child,provider | n/a | 7 ms | expected_failure |
| claude-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | leaf-setsid,mid,provider | n/a | 7 ms | expected_failure |
| claude-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 7 ms | expected_failure |
| claude-u-plain | wait | completed | 0 | yes | NO | none | yes | 37 ms | pass |
| claude-u-child | wait | completed | 0 | yes | NO | none | yes | 136 ms | pass |
| claude-u-setsid | wait | completed | 0 | yes | NO | leaf-setsid | yes | 119 ms | pass |
| claude-u-orphan | wait | completed | 0 | yes | NO | leaf-setsid | yes | 109 ms | pass |
| claude-u-bare | wait | completed | 0 | yes | NO | bare | yes | 421 ms | pass |
| claude-ud-plain | wait | timed_out | 124 | yes | NO | none | yes | 38 ms | pass |
| claude-ud-child | wait | timed_out | 124 | yes | NO | none | yes | 38 ms | pass |
| claude-ud-setsid | wait | timed_out | 124 | yes | NO | leaf-setsid | yes | 14 ms | pass |
| claude-ud-stubborn | wait | timed_out | 124 | yes | NO | none | yes | 1519 ms | pass |
| claude-ui-plain | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 37 ms | pass |
| claude-ui-child | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 35 ms | pass |
| claude-ui-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | NO | leaf-setsid | yes | 28 ms | pass |
| claude-ui-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 30 ms | pass |
| codex-a-plain | wait | completed | 0 | yes | yes | none | yes | 263 ms | pass |
| codex-a-child | wait | completed | 0 | yes | yes | none | yes | 276 ms | pass |
| codex-a-setsid | wait | completed | 0 | yes | yes | none | yes | 269 ms | pass |
| codex-a-ignore | wait | completed | 0 | yes | yes | none | yes | 1751 ms | pass |
| codex-a-stubborn | wait | completed | 0 | yes | yes | none | yes | 187 ms | pass |
| codex-a-pipe | wait | completed | 0 | yes | yes | none | yes | 293 ms | pass |
| codex-a-orphan | wait | completed | 0 | yes | yes | leaf-setsid | NO | 286 ms | expected_failure |
| codex-a-orphan-late | wait | completed | 0 | yes | yes | none | yes | 296 ms | pass |
| codex-a-relay | wait | completed | 0 | yes | yes | relay-heartbeat | NO | 272 ms | expected_failure |
| codex-a-bare | wait | completed | 0 | yes | yes | bare | NO | 603 ms | expected_failure |
| codex-b-plain | wait | failed | 1 | yes | yes | none | yes | 200 ms | pass |
| codex-b-child | wait | failed | 1 | yes | yes | none | yes | 318 ms | pass |
| codex-b-setsid | wait | failed | 1 | yes | yes | none | yes | 314 ms | pass |
| codex-c-plain | wait | timed_out | 124 | yes | yes | none | yes | 99 ms | pass |
| codex-c-child | wait | timed_out | 124 | yes | yes | none | yes | 102 ms | pass |
| codex-c-setsid | wait | timed_out | 124 | yes | yes | none | yes | 96 ms | pass |
| codex-c-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1641 ms | pass |
| codex-c-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1656 ms | pass |
| codex-c-pipe | wait | timed_out | 124 | yes | yes | none | yes | 112 ms | pass |
| codex-c-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 106 ms | expected_failure |
| codex-c-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 103 ms | pass |
| codex-c-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 115 ms | expected_failure |
| codex-c-bare | wait | timed_out | 124 | yes | yes | none | yes | 96 ms | pass |
| codex-d-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 105 ms | pass |
| codex-d-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 104 ms | pass |
| codex-d-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 89 ms | pass |
| codex-d-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1594 ms | pass |
| codex-d-stubborn | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1624 ms | pass |
| codex-d-pipe | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 114 ms | pass |
| codex-d-orphan | SIGINT at 1000 ms | interrupted | 130 | yes | yes | leaf-setsid | NO | 92 ms | expected_failure |
| codex-d-orphan-late | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 114 ms | pass |
| codex-d-relay | SIGINT at 1000 ms | interrupted | 130 | yes | yes | relay-heartbeat | NO | 115 ms | expected_failure |
| codex-d-bare | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 101 ms | pass |
| codex-dt-plain | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 115 ms | pass |
| codex-dt-child | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 84 ms | pass |
| codex-dt-setsid | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 89 ms | pass |
| codex-dt-stubborn | SIGTERM at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1630 ms | pass |
| codex-dh-plain | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 113 ms | pass |
| codex-dh-child | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 104 ms | pass |
| codex-dh-setsid | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 92 ms | pass |
| codex-dh-stubborn | SIGHUP at 1000 ms | interrupted | 130 | yes | yes | none | yes | 1598 ms | pass |
| codex-e-plain | wait | failed | 1 | yes | yes | none | yes | 756 ms | pass |
| codex-e-child | wait | failed | 1 | yes | yes | none | yes | 808 ms | pass |
| codex-e-setsid | wait | failed | 1 | yes | yes | none | yes | 822 ms | pass |
| codex-e-ignore | wait | failed | 1 | yes | yes | none | yes | 2298 ms | pass |
| codex-e-pipe | wait | failed | 1 | yes | yes | none | yes | 801 ms | pass |
| codex-e-orphan | wait | failed | 1 | yes | yes | leaf-setsid | NO | 810 ms | expected_failure |
| codex-e-orphan-late | wait | failed | 1 | yes | yes | none | yes | 791 ms | pass |
| codex-e-relay | wait | failed | 1 | yes | yes | relay-heartbeat | NO | 791 ms | expected_failure |
| codex-e-bare | wait | failed | 1 | yes | yes | none | yes | 1803 ms | pass |
| codex-ed-plain | wait | failed | 1 | yes | yes | none | yes | 749 ms | pass |
| codex-ed-pipe | wait | failed | 1 | yes | yes | none | yes | 792 ms | pass |
| codex-fd-plain | wait | timed_out | 124 | yes | yes | none | yes | 89 ms | pass |
| codex-fd-child | wait | timed_out | 124 | yes | yes | none | yes | 97 ms | pass |
| codex-fd-setsid | wait | timed_out | 124 | yes | yes | none | yes | 108 ms | pass |
| codex-fd-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1604 ms | pass |
| codex-fd-stubborn | wait | timed_out | 124 | yes | yes | none | yes | 1650 ms | pass |
| codex-fd-pipe | wait | timed_out | 124 | yes | yes | none | yes | 100 ms | pass |
| codex-fd-orphan | wait | timed_out | 124 | yes | yes | leaf-setsid | NO | 104 ms | expected_failure |
| codex-fd-orphan-late | wait | timed_out | 124 | yes | yes | none | yes | 89 ms | pass |
| codex-fd-relay | wait | timed_out | 124 | yes | yes | relay-heartbeat | NO | 111 ms | expected_failure |
| codex-fd-bare | wait | timed_out | 124 | yes | yes | none | yes | 92 ms | pass |
| codex-hd-plain | wait | timed_out | 124 | yes | yes | none | yes | 94 ms | pass |
| codex-hd-child | wait | timed_out | 124 | yes | yes | none | yes | 122 ms | pass |
| codex-i-plain | wait | failed | 1 | yes | yes | none | yes | 172 ms | pass |
| codex-i-child | wait | failed | 1 | yes | yes | none | yes | 284 ms | pass |
| codex-i-setsid | wait | failed | 1 | yes | yes | none | yes | 274 ms | pass |
| codex-id-plain | wait | failed | 1 | yes | yes | none | yes | 192 ms | pass |
| codex-fl-plain | wait | timed_out | 124 | yes | yes | none | yes | 334 ms | pass |
| codex-fl-child | wait | timed_out | 124 | yes | yes | none | yes | 380 ms | pass |
| codex-fl-ignore | wait | timed_out | 124 | yes | yes | none | yes | 1763 ms | pass |
| codex-fli-plain | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 181 ms | pass |
| codex-fli-child | SIGINT at 1000 ms | interrupted | 130 | yes | yes | none | yes | 230 ms | pass |
| codex-dc-plain | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 120 ms | pass |
| codex-dc-child | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 134 ms | pass |
| codex-dc-setsid | close stdout at 1000 ms | none | 124 | n/a | n/a | none | n/a | 112 ms | pass |
| codex-dca-plain | close stdout at 300 ms | none | 0 | n/a | n/a | none | n/a | 1765 ms | pass |
| codex-k-plain | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 7 ms | expected_failure |
| codex-k-child | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | child,provider | n/a | 7 ms | expected_failure |
| codex-k-setsid | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | leaf-setsid,mid,provider | n/a | 7 ms | expected_failure |
| codex-k-stubborn | SIGKILL at 1000 ms | none | signal 9 | n/a | n/a | provider | n/a | 5 ms | expected_failure |
| codex-u-plain | wait | completed | 0 | yes | NO | none | yes | 63 ms | pass |
| codex-u-child | wait | completed | 0 | yes | NO | none | yes | 123 ms | pass |
| codex-u-setsid | wait | completed | 0 | yes | NO | leaf-setsid | yes | 131 ms | pass |
| codex-u-orphan | wait | completed | 0 | yes | NO | leaf-setsid | yes | 116 ms | pass |
| codex-u-bare | wait | completed | 0 | yes | NO | bare | yes | 424 ms | pass |
| codex-ud-plain | wait | timed_out | 124 | yes | NO | none | yes | 40 ms | pass |
| codex-ud-child | wait | timed_out | 124 | yes | NO | none | yes | 38 ms | pass |
| codex-ud-setsid | wait | timed_out | 124 | yes | NO | leaf-setsid | yes | 33 ms | pass |
| codex-ud-stubborn | wait | timed_out | 124 | yes | NO | none | yes | 1533 ms | pass |
| codex-ui-plain | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 36 ms | pass |
| codex-ui-child | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 27 ms | pass |
| codex-ui-setsid | SIGINT at 1000 ms | interrupted | 130 | yes | NO | leaf-setsid | yes | 29 ms | pass |
| codex-ui-ignore | SIGINT at 1000 ms | interrupted | 130 | yes | NO | none | yes | 30 ms | pass |

## Live suite

Acceptance criterion 6 asks for a small live suite per provider. `--live` runs it against the
installed `claude` and `codex` (README.md, "Live suite"); this section is generated from the
recorded run in `live.jsonl` and `live.meta.json`.

No live run is recorded.

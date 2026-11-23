# Comparison: delega against the spike's counterpart and due numbers

Generated from `results.jsonl` and `spike-numbers.jsonl` by `cargo run -p delega-acceptance -- --report`;
do not edit by hand. The spike numbers are copied from the committed `results.jsonl` files of the outcome
spike (`delega-dot-spike-outcomes`, commit `7f4da23`): counterpart at its commit `8fa4645` and due at its
commit `8d56df2`. Only the status, the time to the result and the survivors are copied.

## What is and is not comparable

- The spike drove two libraries from a harness process (counterpart's supervisor, due's engine). This
harness drives the `delega` executable: a caller action is an operating system signal or a closed pipe,
and the result is the JSON on standard output and the exit status.
- Times are from start to exit in milliseconds, as the spike's `elapsed_ms` is from the start of the call.
A `delega` run also pays process start and one provider probe for Claude and two for Codex.
- Survivors: the spike measured 5 s after the result (after close and shutdown for resident workers and
counterpart; `*` marks a process that was alive 5 s after the result and removed only by close). This
harness measures 1 s after `delega` exits, because `delega` waits for cleanup before it exits.
- The cases are not the spike's one for one. Rows are compared where the provider behaviour, the variant and
the caller action carry over: cases `a`, `b`, `c`, `d`, `e`, `ed`, `fd`, `hd`, `i`, `id`. The spike's `d` is
a library `interrupt` at 1 s; here it is SIGINT to `delega` at 1 s.
- The fakes differ: these are the stream-accurate fakes `delega` already tests against, extended with the
spike's process behaviours. The descendant variants are ported; the provider scripts are not the spike's
byte for byte.
- The delega run is on a release build with 8 cases at a time; the spike ran 12 at a time.

## Summary on the shared rows

For each side, the rows that both this harness and that side ran. C1: a stated outcome came back (a
valid result on standard output for delega; completed, failed, timed out, cancelled or interrupted for the
spike). C2: nothing of the case alive at the measurement. A stated outcome says nothing about whether it
was the right one.

| Side | Shared rows | C1 met | C2 met | Both met |
| --- | --- | --- | --- | --- |
| counterpart | 72 | 72 | 65 | 65 |
| delega, those 72 rows | 72 | 72 | 66 | 66 |
| due one-shot | 60 | 58 | 48 | 46 |
| delega, those 60 rows | 60 | 60 | 60 | 60 |
| due resident | 60 | 54 | 42 | 37 |
| delega, those 60 rows | 60 | 60 | 60 | 60 |

## Timing on the common rows

Time to the result in milliseconds, start to exit. `c` is a 2 s time limit, `d` an interrupt at 1 s, `ignore` a child that ignores TERM and INT, `stubborn` a provider that does.

| Row | delega | counterpart | due one-shot | due resident |
| --- | --- | --- | --- | --- |
| claude-a-plain | 687 | 64 | 27 | 29 |
| claude-c-plain | 2166 | 2074 | 4004 | 2005 |
| claude-c-ignore | 3666 | 3593 | 4004 | 2005 |
| claude-c-stubborn | 3701 | 3572 | 4006 | 8006 |
| claude-d-plain | 1142 | 1057 | 3004 | 1009 |
| claude-d-stubborn | 2685 | 2557 | 3004 | 4004 |
| claude-e-plain | 772 | 787 | 535 | 2547 |
| claude-e-pipe | 843 | 568 | 30002 | 30002 |
| codex-a-plain | 301 | 130 | 28 | 29 |
| codex-c-plain | 2148 | 2060 | 4004 | 2005 |
| codex-c-ignore | 3670 | 3581 | 4004 | 2010 |
| codex-c-stubborn | 3695 | 3560 | 4005 | 8006 |
| codex-d-plain | 1134 | 1052 | 3004 | 1009 |
| codex-d-stubborn | 2702 | 2554 | 3004 | 4004 |
| codex-e-plain | 843 | 612 | 533 | 534 |
| codex-e-pipe | 921 | 631 | 30002 | 30001 |

## Row by row

Each cell is the outcome (or status), the milliseconds to the result, and what was left. `-` means the side did not run the row.

| Row | delega | counterpart | due one-shot | due resident |
| --- | --- | --- | --- | --- |
| claude-a-plain | completed 687 ms; left none | completed 64 ms; left none | Succeeded 27 ms; left none | Succeeded 29 ms; left none* |
| claude-b-plain | failed 233 ms; left none | failed 64 ms; left none | Failed 33 ms; left none | Failed 2033 ms; left none |
| claude-c-plain | timed_out 2166 ms; left none | failed (timeout) 2074 ms; left none | TimedOut 4004 ms; left none | TimedOut 2005 ms; left none* |
| claude-c-child | timed_out 2158 ms; left none | failed (timeout) 2053 ms; left none | TimedOut 4004 ms; left none | TimedOut 2006 ms; left none* |
| claude-c-setsid | timed_out 2153 ms; left none | failed (timeout) 2120 ms; left none | TimedOut 4003 ms; left leaf-setsid | TimedOut 2007 ms; left leaf-setsid |
| claude-c-ignore | timed_out 3666 ms; left none | failed (timeout) 3593 ms; left none | TimedOut 4004 ms; left none | TimedOut 2005 ms; left none* |
| claude-c-stubborn | timed_out 3701 ms; left none | failed (timeout) 3572 ms; left none | TimedOut 4006 ms; left none | Failed 8006 ms; left none* |
| claude-c-pipe | timed_out 2158 ms; left none | failed (timeout) 2072 ms; left none | TimedOut 4005 ms; left none | TimedOut 2005 ms; left none* |
| claude-c-orphan | timed_out 2154 ms; left leaf-setsid | failed (timeout) 2056 ms; left leaf-setsid | - | - |
| claude-c-orphan-late | timed_out 2140 ms; left none | failed (timeout) 2123 ms; left none | - | - |
| claude-d-plain | interrupted 1142 ms; left none | interrupted 1057 ms; left none | Cancelled 3004 ms; left none | Cancelled 1009 ms; left none* |
| claude-d-child | interrupted 1160 ms; left none | interrupted 1057 ms; left none | Cancelled 3004 ms; left none | Cancelled 1005 ms; left none* |
| claude-d-setsid | interrupted 1170 ms; left none | interrupted 1118 ms; left none | Cancelled 3004 ms; left leaf-setsid | Cancelled 1005 ms; left leaf-setsid |
| claude-d-ignore | interrupted 2652 ms; left none | interrupted 2571 ms; left none | Cancelled 3004 ms; left none | Cancelled 1007 ms; left none* |
| claude-d-stubborn | interrupted 2685 ms; left none | interrupted 2557 ms; left none | Cancelled 3004 ms; left none | NO RESULT interrupt-error 4004 ms; left none* |
| claude-d-pipe | interrupted 1160 ms; left none | interrupted 1063 ms; left none | Cancelled 3004 ms; left none | Cancelled 1010 ms; left none* |
| claude-d-orphan | interrupted 1172 ms; left leaf-setsid | interrupted 1065 ms; left leaf-setsid | - | - |
| claude-d-orphan-late | interrupted 1148 ms; left none | interrupted 1125 ms; left none | - | - |
| claude-e-plain | failed 772 ms; left none | failed 787 ms; left none | Failed 535 ms; left none | Failed 2547 ms; left none |
| claude-e-child | failed 820 ms; left none | failed 680 ms; left none | Failed 526 ms; left child | Failed 2536 ms; left none |
| claude-e-setsid | failed 833 ms; left none | failed 915 ms; left none | Failed 567 ms; left leaf-setsid,mid | Failed 2540 ms; left leaf-setsid |
| claude-e-ignore | failed 2322 ms; left none | failed 2100 ms; left none | Failed 536 ms; left child-ignore | Failed 2539 ms; left none |
| claude-e-pipe | failed 843 ms; left none | failed 568 ms; left none | NO RESULT HANG 30002 ms; left none | NO RESULT HANG 30002 ms; left none* |
| claude-e-orphan | failed 1180 ms; left leaf-setsid | failed 586 ms; left leaf-setsid | - | - |
| claude-e-orphan-late | failed 847 ms; left none | failed 630 ms; left none | - | - |
| claude-ed-plain | failed 797 ms; left none | failed 588 ms; left none | Failed 521 ms; left none | Failed 2530 ms; left none |
| claude-ed-pipe | failed 830 ms; left none | failed 588 ms; left none | TimedOut 5003 ms; left none | Failed 5003 ms; left none |
| claude-fd-plain | timed_out 3163 ms; left none | failed (timeout) 3054 ms; left none | TimedOut 5005 ms; left none | Failed 9007 ms; left none* |
| claude-fd-child | timed_out 3148 ms; left none | failed (timeout) 3064 ms; left none | TimedOut 5004 ms; left none | Failed 9007 ms; left none* |
| claude-fd-setsid | timed_out 3155 ms; left none | failed (timeout) 3122 ms; left none | TimedOut 5004 ms; left leaf-setsid | Failed 9006 ms; left leaf-setsid |
| claude-fd-ignore | timed_out 4651 ms; left none | failed (timeout) 4566 ms; left none | TimedOut 5004 ms; left none | Failed 9006 ms; left none* |
| claude-fd-stubborn | timed_out 4714 ms; left none | failed (timeout) 4573 ms; left none | TimedOut 5005 ms; left none | Failed 9005 ms; left none* |
| claude-fd-pipe | timed_out 3170 ms; left none | failed (timeout) 3060 ms; left none | TimedOut 5004 ms; left none | Failed 9006 ms; left none* |
| claude-hd-plain | timed_out 3161 ms; left none | failed (timeout) 3063 ms; left none | TimedOut 5003 ms; left none | Failed 6004 ms; left none |
| claude-i-plain | failed 211 ms; left none | failed 65 ms; left none | Failed 318 ms; left none | NO RESULT HANG 12002 ms; left none* |
| claude-id-plain | failed 221 ms; left none | failed 86 ms; left none | Failed 182 ms; left none | TimedOut 3005 ms; left none* |
| codex-a-plain | completed 301 ms; left none | completed 130 ms; left none | Succeeded 28 ms; left none | Succeeded 29 ms; left none* |
| codex-b-plain | failed 306 ms; left none | failed 130 ms; left none | Failed 27 ms; left none | Failed 24 ms; left none |
| codex-c-plain | timed_out 2148 ms; left none | failed (timeout) 2060 ms; left none | TimedOut 4004 ms; left none | TimedOut 2005 ms; left none* |
| codex-c-child | timed_out 2143 ms; left none | failed (timeout) 2053 ms; left none | TimedOut 4004 ms; left none | TimedOut 2006 ms; left child |
| codex-c-setsid | timed_out 2142 ms; left none | failed (timeout) 2137 ms; left none | TimedOut 4004 ms; left leaf-setsid | TimedOut 2005 ms; left leaf-setsid,mid |
| codex-c-ignore | timed_out 3670 ms; left none | failed (timeout) 3581 ms; left none | TimedOut 4004 ms; left none | TimedOut 2010 ms; left child-ignore |
| codex-c-stubborn | timed_out 3695 ms; left none | failed (timeout) 3560 ms; left none | TimedOut 4005 ms; left none | Failed 8006 ms; left none* |
| codex-c-pipe | timed_out 2166 ms; left none | failed (timeout) 2073 ms; left none | TimedOut 4004 ms; left none | TimedOut 2005 ms; left child |
| codex-c-orphan | timed_out 2167 ms; left leaf-setsid | failed (timeout) 2056 ms; left leaf-setsid | - | - |
| codex-c-orphan-late | timed_out 2141 ms; left none | failed (timeout) 2119 ms; left none | - | - |
| codex-d-plain | interrupted 1134 ms; left none | interrupted 1052 ms; left none | Cancelled 3004 ms; left none | Cancelled 1009 ms; left none* |
| codex-d-child | interrupted 1145 ms; left none | interrupted 1051 ms; left none | Cancelled 3005 ms; left none | Cancelled 1005 ms; left child |
| codex-d-setsid | interrupted 1158 ms; left none | interrupted 1112 ms; left none | Cancelled 3004 ms; left leaf-setsid | Cancelled 1005 ms; left leaf-setsid,mid |
| codex-d-ignore | interrupted 2686 ms; left none | interrupted 2550 ms; left none | Cancelled 3003 ms; left none | Cancelled 1009 ms; left child-ignore |
| codex-d-stubborn | interrupted 2702 ms; left none | interrupted 2554 ms; left none | Cancelled 3004 ms; left none | NO RESULT interrupt-error 4004 ms; left none* |
| codex-d-pipe | interrupted 1182 ms; left none | interrupted 1053 ms; left none | Cancelled 3003 ms; left none | Cancelled 1006 ms; left child |
| codex-d-orphan | interrupted 1192 ms; left leaf-setsid | interrupted 1058 ms; left leaf-setsid | - | - |
| codex-d-orphan-late | interrupted 1158 ms; left none | interrupted 1106 ms; left none | - | - |
| codex-e-plain | failed 843 ms; left none | failed 612 ms; left none | Failed 533 ms; left none | Failed 534 ms; left none |
| codex-e-child | failed 939 ms; left none | failed 627 ms; left none | Failed 541 ms; left child | Failed 528 ms; left child |
| codex-e-setsid | failed 935 ms; left none | failed 698 ms; left none | Failed 566 ms; left leaf-setsid,mid | Failed 533 ms; left leaf-setsid,mid |
| codex-e-ignore | failed 2410 ms; left none | failed 2119 ms; left none | Failed 536 ms; left child-ignore | Failed 620 ms; left child-ignore |
| codex-e-pipe | failed 921 ms; left none | failed 631 ms; left none | NO RESULT HANG 30002 ms; left none | NO RESULT HANG 30001 ms; left child |
| codex-e-orphan | failed 1230 ms; left leaf-setsid | failed 654 ms; left leaf-setsid | - | - |
| codex-e-orphan-late | failed 919 ms; left none | failed 650 ms; left leaf-setsid | - | - |
| codex-ed-plain | failed 885 ms; left none | failed 653 ms; left none | Failed 537 ms; left none | Failed 665 ms; left none |
| codex-ed-pipe | failed 912 ms; left none | failed 631 ms; left none | TimedOut 5004 ms; left none | Failed 6504 ms; left child |
| codex-fd-plain | timed_out 3163 ms; left none | failed (timeout) 3066 ms; left none | TimedOut 5004 ms; left none | Failed 9006 ms; left none* |
| codex-fd-child | timed_out 3165 ms; left none | failed (timeout) 3062 ms; left none | TimedOut 5004 ms; left none | Failed 9006 ms; left none* |
| codex-fd-setsid | timed_out 3147 ms; left none | failed (timeout) 3127 ms; left none | TimedOut 5003 ms; left leaf-setsid | Failed 9006 ms; left leaf-setsid |
| codex-fd-ignore | timed_out 4626 ms; left none | failed (timeout) 4568 ms; left none | TimedOut 5004 ms; left none | Failed 9006 ms; left none* |
| codex-fd-stubborn | timed_out 4708 ms; left none | failed (timeout) 4565 ms; left none | TimedOut 5004 ms; left none | Failed 9006 ms; left none* |
| codex-fd-pipe | timed_out 3161 ms; left none | failed (timeout) 3077 ms; left none | TimedOut 5003 ms; left none | Failed 9006 ms; left none* |
| codex-hd-plain | timed_out 3155 ms; left none | failed (timeout) 3069 ms; left none | TimedOut 5004 ms; left none | Failed 9007 ms; left none |
| codex-i-plain | failed 298 ms; left none | failed 151 ms; left none | Failed 39 ms; left none | NO RESULT HANG 12001 ms; left none* |
| codex-id-plain | failed 313 ms; left none | failed 220 ms; left none | Failed 86 ms; left none | TimedOut 3009 ms; left none* |

72 rows.

## Rows delega runs that the spike did not

130 rows. By case:

- `a` (normal completion): 18 rows.
- `b` (provider exits 3 with an error): 4 rows.
- `c` (timeout 2 s, provider keeps producing output): 4 rows.
- `d` (SIGINT to delega at 1 s): 4 rows.
- `dc` (reader closes delega's stdout at 1 s, timeout 3 s): 6 rows.
- `dca` (reader closes delega's stdout at 0.3 s, provider finishes at 1.5 s): 2 rows.
- `dh` (SIGHUP to delega at 1 s): 8 rows.
- `dt` (SIGTERM to delega at 1 s): 8 rows.
- `e` (provider SIGKILLs itself 0.5 s in): 4 rows.
- `fd` (provider hangs with no output, timeout 3 s): 8 rows.
- `fl` (provider writes output at full speed, timeout 3 s): 6 rows.
- `fli` (provider writes output at full speed, SIGINT at 1 s): 4 rows.
- `hd` (provider closes stdout and stays alive, timeout 3 s): 2 rows.
- `i` (output ends without a final answer, provider exits 0): 4 rows.
- `k` (SIGKILL to delega at 1 s): 20 rows.
- `kk` (SIGKILL to delega and its keeper at 1 s): 2 rows.
- `u` (normal completion, process listing unavailable): 10 rows.
- `ud` (provider hangs, timeout 3 s, process listing unavailable): 8 rows.
- `ui` (SIGINT to delega at 1 s, process listing unavailable): 8 rows.

The spike ran `a` only with the `plain` variant. The `orphan` and `orphan-late` variants were run on counterpart only, so due has no number for them. Cases `dt`, `dh`, `fl`, `fli`, `dc`, `dca` and `k` are new.

## Spike rows not carried over

| Spike case | Why |
| --- | --- |
| f | Provider hangs with no output and no deadline. `delega run` has a default time limit of 600 s, so the row would take ten minutes; `fd` (timeout 3 s) and `k`/`d` cover the hang. |
| h | Provider closes stdout and stays alive with no deadline. Same 600 s default; `hd` covers it with a 3 s timeout. |
| g, gd, gr | A permission asked and nobody answers. `delega run` answers every request no at once (`permissions.on_request` is `deny`); there is no live request to leave unanswered. |
| j | The `wait` future is dropped. The CLI has no future; the nearest caller actions are `dc` (reader closes stdout) and `k` (caller killed). |
| j2 | The handle is dropped. The CLI has no handle; SIGKILL of the caller (`k`) is the nearest row. |

## Measured differences

- On the 72 rows delega shares with counterpart: both left a survivor on `claude-c-orphan`, `claude-d-orphan`, `claude-e-orphan`, `codex-c-orphan`, `codex-d-orphan`, `codex-e-orphan`; only delega on none; only counterpart on `codex-e-orphan-late`.
- Rows where delega's result said cleanup was confirmed while a process of the case was alive: 22. The spike found no counterpart result that named an escaped descendant.
- delega reports a time limit as its own outcome, `timed_out`, with exit status 124 (58 rows); counterpart reported `failed` with `turn_timeout`, due `TimedOut`.

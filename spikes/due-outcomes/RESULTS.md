# Results: does due-core meet criteria 1 and 2?

Criteria, from the repository README:

1. One stated outcome: every run ends in exactly one of completed, failed,
   timed out, interrupted, refused. Never a hang, never silence.
2. No process left behind: after any outcome the provider process and its
   descendants are gone, or the result says cleanup could not be confirmed and
   what remains.

Everything under "Measured" comes from `results.jsonl` in this directory (208
rows, one run each). Everything under "Read from code" or "Judgment" is
labelled as such. The harness and fakes are described in [README.md](README.md).

## Environment

Measured or read from the machine and due's lockfile:

- macOS 27.2.1 (build 26B131), arm64, `/bin/bash` 3.2.57, cargo 1.100.0.
- due commit `8d56df2`, `due-core` 0.2.1, features `oneshot` and `resident`.
- `claude-wrapper` 0.14.11, `codex-wrapper` 0.4.9, `tokio` 1.53.2, from due's
  `Cargo.lock` (copied into this crate).
- 12 rows ran concurrently. Times are wall clock and carry some scheduling
  noise; differences under about 100 ms mean nothing.
- The resident engine ran with `settlement_timeout` set to 3 s (due's default is
  90 s). Times for resident rows that depend on settlement scale with it.

Constants read from the wrapper and due source (not measured):

- One-shot cancel and timeout: the wrapper sends SIGINT to the provider's
  process group, sleeps the full grace period (2 s for both providers), then
  sends SIGKILL to the group. The sleep is unconditional
  (`kill_group_with_grace` in `claude-wrapper/src/exec.rs`).
- Resident Claude: SIGINT, 2 s kill grace, control timeout 5 s. Resident Codex:
  termination grace 5 s, control timeout 30 s.
- A one-shot call without `RunOptions::timeout` and without a firing `cancel`
  future has no deadline of any kind. An assignment without `timeout` has none
  either.

## How the due statuses map to the README outcomes

| README outcome | due result |
| --- | --- |
| completed | `Status::Succeeded` |
| failed | `Status::Failed` (with `ErrorKind` Provider, Protocol, Io, Unsupported, ...) |
| timed out | `Status::TimedOut` (`ErrorKind::Timeout`) |
| interrupted | `Status::Cancelled` (`ErrorKind::Cancelled`) |
| refused | no status. The nearest is `Failed` with `ErrorKind::Unsupported` (reject mode) or an `Err` from an `Engine` call |

## Measured: summary counts

Computed from `results.jsonl`. "Outcome stated" means the call returned a
`RunResult` with status Succeeded, Failed, TimedOut or Cancelled. Rows that end
by dropping the future or the engine (cases `j`, `j2`) return no result by
construction and are counted apart. "Clean at 5 s" means no process started by
the fake provider was alive 5 s after the call returned (one-shot) or after the
result was received (resident). The resident worker stays alive after a run by
design, so the resident "clean at 5 s" figure is low for that reason; the
"clean after shutdown" figure is the one that matches criterion 2 for a caller
that shuts the engine down.

| Path | Provider | Rows | Outcome stated | No outcome (hang, Err, none) | Drop rows | Clean at 5 s | Clean 5 s after `shutdown()` |
| --- | --- | --- | --- | --- | --- | --- | --- |
| one-shot, buffered | claude | 44 | 30 | 8 | 6 | 36 | n/a |
| one-shot, buffered | codex | 44 | 30 | 8 | 6 | 36 | n/a |
| one-shot, streaming | claude | 9 | 7 | 1 | 1 | 9 | n/a |
| one-shot, streaming | codex | 9 | 7 | 1 | 1 | 9 | n/a |
| resident | claude | 51 | 30 | 10 | 11 | 13 | 34 of 46 |
| resident | codex | 51 | 30 | 10 | 11 | 7 | 21 of 46 |

The eight one-shot rows with no outcome are case `f` in six variants, case `h`
and case `e` with the `pipe` variant. The ten resident rows are case `f` in six
variants, `g`, `i`, `e` with `pipe`, and `d` with `stubborn`. In every one of
those rows, due returned nothing the caller could act on until the harness
limit (12 s or 30 s) or, for resident `d` `stubborn`, returned an `Err`.

## Measured: the table

Columns. Outcome received: due's status, then `ErrorKind`, then provider exit
code, then the start of the message. `HANG` means the call had not returned at
the harness limit (12 s for cases with no deadline that are expected to hang,
30 s otherwise); for those rows the survivor columns are measured from the
moment the limit expired, with a one-shot future dropped at that moment and a
resident engine still running. `none: ...` means the caller never receives a
result (cases `j`, `j2`). Time is from the start of the call to the return (for
case `d` it includes the 1 s before the interrupt). Survivors are the roles of
fake-provider processes alive 1 s and 5 s after the call returned: `provider`,
`child`, `child-ignore` (ignores SIGTERM and SIGINT), `mid` and `leaf-setsid`
(the setsid grandchild), `ticker` (the fake's own output generator), and
`(zombie)` for a dead, unreaped process. After shutdown (resident only): the
status of `Engine::shutdown()`, its time in ms, then survivors 1 s and 5 s after
it returned. `[harness killed N leftover]` means N processes were still alive
at the end of the row and the harness killed them.

Case letters: `a` normal, `b` provider exits 3, `c` timeout 2 s while output
continues, `d` caller interrupts at 1 s, `e` provider SIGKILLed (`ed` with a 3 s
timeout), `f` silent hang (`fd` with a 3 s timeout), `g` permission asked and
nobody answers (`gd` with a 3 s timeout, `gr` reject mode), `h` stdout closed
while alive (`hd` with a 3 s timeout), `i` output ends without a final result
(`id` with a 3 s timeout), `j` calling task cancelled or `wait` future dropped,
`j2` the engine dropped. Variants: `plain`, `child`, `setsid`, `ignore`,
`stubborn`, `pipe`, as in the README. Messages are cut to 70 characters; the
full text is in `results.jsonl`.

| Path | Provider | Case | Variant | Outcome received | Time (ms) | Survivors 1 s | Survivors 5 s | After shutdown (time ms; 1 s; 5 s) | Note |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| oneshot (stream) | claude | a | plain | Succeeded | 32 | none | none | n/a |  |
| oneshot (stream) | claude | b | plain | Failed / Provider (exit 3): fake claude: fatal error | 28 | none | none | n/a |  |
| oneshot (stream) | claude | c | plain | TimedOut / Timeout: Claude exceeded its execution timeout (2s) | 4004 | none | none | n/a |  |
| oneshot (stream) | claude | d | plain | Cancelled / Cancelled: Claude run cancelled | 3004 | none | none | n/a | cancel future fired at 1 s |
| oneshot (stream) | claude | e | plain | Failed / Provider (exit -1): Claude exited with status -1 | 534 | none | none | n/a |  |
| oneshot (stream) | claude | fd | plain | TimedOut / Timeout: Claude exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot (stream) | claude | h | plain | HANG | 12003 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot (stream) | claude | i | plain | Failed / Protocol: Claude stream has no terminal result | 108 | none | none | n/a |  |
| oneshot (stream) | claude | j | plain | none: JoinError cancelled=true; no RunResult | 1003 | none | none | n/a | calling task aborted at 1 s |
| oneshot (stream) | codex | a | plain | Succeeded | 104 | none | none | n/a |  |
| oneshot (stream) | codex | b | plain | Failed / Provider (exit 3): fake codex: fatal error | 104 | none | none | n/a |  |
| oneshot (stream) | codex | c | plain | TimedOut / Timeout: Codex exceeded its execution timeout (2s) | 4003 | none | none | n/a |  |
| oneshot (stream) | codex | d | plain | Cancelled / Cancelled: Codex run cancelled | 3004 | none | none | n/a | cancel future fired at 1 s |
| oneshot (stream) | codex | e | plain | Failed / Provider (exit -1): Codex exited with status -1 | 612 | none | none | n/a |  |
| oneshot (stream) | codex | fd | plain | TimedOut / Timeout: Codex exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot (stream) | codex | h | plain | HANG | 12002 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot (stream) | codex | i | plain | Failed / Protocol: Codex did not emit turn.completed | 108 | none | none | n/a |  |
| oneshot (stream) | codex | j | plain | none: JoinError cancelled=true; no RunResult | 1002 | none | none | n/a | calling task aborted at 1 s |
| oneshot | claude | a | plain | Succeeded | 27 | none | none | n/a |  |
| oneshot | claude | b | plain | Failed / Provider (exit 3): fake claude: fatal error | 33 | none | none | n/a |  |
| oneshot | claude | c | child | TimedOut / Timeout: Claude exceeded its execution timeout (2s) | 4004 | none | none | n/a |  |
| oneshot | claude | c | ignore | TimedOut / Timeout: Claude exceeded its execution timeout (2s) | 4004 | none | none | n/a |  |
| oneshot | claude | c | pipe | TimedOut / Timeout: Claude exceeded its execution timeout (2s) | 4005 | none | none | n/a |  |
| oneshot | claude | c | plain | TimedOut / Timeout: Claude exceeded its execution timeout (2s) | 4004 | none | none | n/a |  |
| oneshot | claude | c | setsid | TimedOut / Timeout: Claude exceeded its execution timeout (2s) | 4003 | leaf-setsid | leaf-setsid | n/a | [harness killed 1 leftover] |
| oneshot | claude | c | stubborn | TimedOut / Timeout: Claude exceeded its execution timeout (2s) | 4006 | none | none | n/a |  |
| oneshot | claude | d | child | Cancelled / Cancelled: Claude run cancelled | 3004 | none | none | n/a | cancel future fired at 1 s |
| oneshot | claude | d | ignore | Cancelled / Cancelled: Claude run cancelled | 3004 | none | none | n/a | cancel future fired at 1 s |
| oneshot | claude | d | pipe | Cancelled / Cancelled: Claude run cancelled | 3004 | none | none | n/a | cancel future fired at 1 s |
| oneshot | claude | d | plain | Cancelled / Cancelled: Claude run cancelled | 3004 | none | none | n/a | cancel future fired at 1 s |
| oneshot | claude | d | setsid | Cancelled / Cancelled: Claude run cancelled | 3004 | leaf-setsid | leaf-setsid | n/a | cancel future fired at 1 s [harness killed 1 leftover] |
| oneshot | claude | d | stubborn | Cancelled / Cancelled: Claude run cancelled | 3004 | none | none | n/a | cancel future fired at 1 s |
| oneshot | claude | e | child | Failed / Provider (exit -1): Claude exited with status -1 | 526 | child | child | n/a | [harness killed 1 leftover] |
| oneshot | claude | e | ignore | Failed / Provider (exit -1): Claude exited with status -1 | 536 | child-ignore | child-ignore | n/a | [harness killed 1 leftover] |
| oneshot | claude | e | pipe | HANG | 30002 | none | none | n/a | no result in 30 s; future dropped at the limit |
| oneshot | claude | e | plain | Failed / Provider (exit -1): Claude exited with status -1 | 535 | none | none | n/a |  |
| oneshot | claude | e | setsid | Failed / Provider (exit -1): Claude exited with status -1 | 567 | leaf-setsid,mid | leaf-setsid,mid | n/a | [harness killed 2 leftover] |
| oneshot | claude | ed | pipe | TimedOut / Timeout: Claude exceeded its execution timeout (3s) | 5003 | none | none | n/a |  |
| oneshot | claude | ed | plain | Failed / Provider (exit -1): Claude exited with status -1 | 521 | none | none | n/a |  |
| oneshot | claude | f | child | HANG | 12002 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | claude | f | ignore | HANG | 12001 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | claude | f | pipe | HANG | 12002 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | claude | f | plain | HANG | 12002 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | claude | f | setsid | HANG | 12002 | leaf-setsid | leaf-setsid | n/a | no result in 12 s; future dropped at the limit [harness killed 1 leftover] |
| oneshot | claude | f | stubborn | HANG | 12001 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | claude | fd | child | TimedOut / Timeout: Claude exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot | claude | fd | ignore | TimedOut / Timeout: Claude exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot | claude | fd | pipe | TimedOut / Timeout: Claude exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot | claude | fd | plain | TimedOut / Timeout: Claude exceeded its execution timeout (3s) | 5005 | none | none | n/a |  |
| oneshot | claude | fd | setsid | TimedOut / Timeout: Claude exceeded its execution timeout (3s) | 5004 | leaf-setsid | leaf-setsid | n/a | [harness killed 1 leftover] |
| oneshot | claude | fd | stubborn | TimedOut / Timeout: Claude exceeded its execution timeout (3s) | 5005 | none | none | n/a |  |
| oneshot | claude | g | plain | Succeeded | 88 | none | none | n/a |  |
| oneshot | claude | h | plain | HANG | 12002 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | claude | hd | plain | TimedOut / Timeout: Claude exceeded its execution timeout (3s) | 5003 | none | none | n/a |  |
| oneshot | claude | i | plain | Failed / Protocol: Claude transcript has no terminal result | 318 | none | none | n/a |  |
| oneshot | claude | id | plain | Failed / Protocol: Claude transcript has no terminal result | 182 | none | none | n/a |  |
| oneshot | claude | j | child | none: JoinError cancelled=true; no RunResult | 1002 | none | none | n/a | calling task aborted at 1 s |
| oneshot | claude | j | ignore | none: JoinError cancelled=true; no RunResult | 1002 | none | none | n/a | calling task aborted at 1 s |
| oneshot | claude | j | pipe | none: JoinError cancelled=true; no RunResult | 1002 | none | none | n/a | calling task aborted at 1 s |
| oneshot | claude | j | plain | none: JoinError cancelled=true; no RunResult | 1002 | none | none | n/a | calling task aborted at 1 s |
| oneshot | claude | j | setsid | none: JoinError cancelled=true; no RunResult | 1002 | leaf-setsid | leaf-setsid | n/a | calling task aborted at 1 s [harness killed 1 leftover] |
| oneshot | claude | j | stubborn | none: JoinError cancelled=true; no RunResult | 1002 | none | none | n/a | calling task aborted at 1 s |
| oneshot | codex | a | plain | Succeeded | 28 | none | none | n/a |  |
| oneshot | codex | b | plain | Failed / Provider (exit 3): fake codex: fatal error | 27 | none | none | n/a |  |
| oneshot | codex | c | child | TimedOut / Timeout: Codex exceeded its execution timeout (2s) | 4004 | none | none | n/a |  |
| oneshot | codex | c | ignore | TimedOut / Timeout: Codex exceeded its execution timeout (2s) | 4004 | none | none | n/a |  |
| oneshot | codex | c | pipe | TimedOut / Timeout: Codex exceeded its execution timeout (2s) | 4004 | none | none | n/a |  |
| oneshot | codex | c | plain | TimedOut / Timeout: Codex exceeded its execution timeout (2s) | 4004 | none | none | n/a |  |
| oneshot | codex | c | setsid | TimedOut / Timeout: Codex exceeded its execution timeout (2s) | 4004 | leaf-setsid | leaf-setsid | n/a | [harness killed 1 leftover] |
| oneshot | codex | c | stubborn | TimedOut / Timeout: Codex exceeded its execution timeout (2s) | 4005 | none | none | n/a |  |
| oneshot | codex | d | child | Cancelled / Cancelled: Codex run cancelled | 3005 | none | none | n/a | cancel future fired at 1 s |
| oneshot | codex | d | ignore | Cancelled / Cancelled: Codex run cancelled | 3003 | none | none | n/a | cancel future fired at 1 s |
| oneshot | codex | d | pipe | Cancelled / Cancelled: Codex run cancelled | 3003 | none | none | n/a | cancel future fired at 1 s |
| oneshot | codex | d | plain | Cancelled / Cancelled: Codex run cancelled | 3004 | none | none | n/a | cancel future fired at 1 s |
| oneshot | codex | d | setsid | Cancelled / Cancelled: Codex run cancelled | 3004 | leaf-setsid | leaf-setsid | n/a | cancel future fired at 1 s [harness killed 1 leftover] |
| oneshot | codex | d | stubborn | Cancelled / Cancelled: Codex run cancelled | 3004 | none | none | n/a | cancel future fired at 1 s |
| oneshot | codex | e | child | Failed / Provider (exit -1): Codex exited with status -1 | 541 | child | child | n/a | [harness killed 1 leftover] |
| oneshot | codex | e | ignore | Failed / Provider (exit -1): Codex exited with status -1 | 536 | child-ignore | child-ignore | n/a | [harness killed 1 leftover] |
| oneshot | codex | e | pipe | HANG | 30002 | none | none | n/a | no result in 30 s; future dropped at the limit |
| oneshot | codex | e | plain | Failed / Provider (exit -1): Codex exited with status -1 | 533 | none | none | n/a |  |
| oneshot | codex | e | setsid | Failed / Provider (exit -1): Codex exited with status -1 | 566 | leaf-setsid,mid | leaf-setsid,mid | n/a | [harness killed 2 leftover] |
| oneshot | codex | ed | pipe | TimedOut / Timeout: Codex exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot | codex | ed | plain | Failed / Provider (exit -1): Codex exited with status -1 | 537 | none | none | n/a |  |
| oneshot | codex | f | child | HANG | 12003 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | codex | f | ignore | HANG | 12002 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | codex | f | pipe | HANG | 12002 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | codex | f | plain | HANG | 12002 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | codex | f | setsid | HANG | 12002 | leaf-setsid | leaf-setsid | n/a | no result in 12 s; future dropped at the limit [harness killed 1 leftover] |
| oneshot | codex | f | stubborn | HANG | 12002 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | codex | fd | child | TimedOut / Timeout: Codex exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot | codex | fd | ignore | TimedOut / Timeout: Codex exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot | codex | fd | pipe | TimedOut / Timeout: Codex exceeded its execution timeout (3s) | 5003 | none | none | n/a |  |
| oneshot | codex | fd | plain | TimedOut / Timeout: Codex exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot | codex | fd | setsid | TimedOut / Timeout: Codex exceeded its execution timeout (3s) | 5003 | leaf-setsid | leaf-setsid | n/a | [harness killed 1 leftover] |
| oneshot | codex | fd | stubborn | TimedOut / Timeout: Codex exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot | codex | g | plain | Succeeded | 30 | none | none | n/a |  |
| oneshot | codex | h | plain | HANG | 12002 | none | none | n/a | no result in 12 s; future dropped at the limit |
| oneshot | codex | hd | plain | TimedOut / Timeout: Codex exceeded its execution timeout (3s) | 5004 | none | none | n/a |  |
| oneshot | codex | i | plain | Failed / Protocol: Codex did not emit turn.completed | 39 | none | none | n/a |  |
| oneshot | codex | id | plain | Failed / Protocol: Codex did not emit turn.completed | 86 | none | none | n/a |  |
| oneshot | codex | j | child | none: JoinError cancelled=true; no RunResult | 1002 | none | none | n/a | calling task aborted at 1 s |
| oneshot | codex | j | ignore | none: JoinError cancelled=true; no RunResult | 1002 | none | none | n/a | calling task aborted at 1 s |
| oneshot | codex | j | pipe | none: JoinError cancelled=true; no RunResult | 1002 | none | none | n/a | calling task aborted at 1 s |
| oneshot | codex | j | plain | none: JoinError cancelled=true; no RunResult | 1003 | none | none | n/a | calling task aborted at 1 s |
| oneshot | codex | j | setsid | none: JoinError cancelled=true; no RunResult | 1003 | leaf-setsid | leaf-setsid | n/a | calling task aborted at 1 s [harness killed 1 leftover] |
| oneshot | codex | j | stubborn | none: JoinError cancelled=true; no RunResult | 1003 | none | none | n/a | calling task aborted at 1 s |
| resident | claude | a | plain | Succeeded | 29 | provider | provider | shutdown-ok; 2013; none; none |  |
| resident | claude | b | plain | Failed / Protocol: native failure cleanup: duplex child exited unsuccessfully (exit statu... | 2033 | none | none | shutdown-error; 0; none; none |  |
| resident | claude | c | child | TimedOut / Timeout: assignment deadline exceeded | 2006 | child,provider,ticker | child,provider,ticker | shutdown-ok; 2014; none; none |  |
| resident | claude | c | ignore | TimedOut / Timeout: assignment deadline exceeded | 2005 | child-ignore,provider,ticker | child-ignore,provider,ticker | shutdown-ok; 2014; none; none |  |
| resident | claude | c | pipe | TimedOut / Timeout: assignment deadline exceeded | 2005 | child,provider,ticker | child,provider,ticker | shutdown-ok; 2015; none; none |  |
| resident | claude | c | plain | TimedOut / Timeout: assignment deadline exceeded | 2005 | provider,ticker | provider,ticker | shutdown-ok; 2014; none; none |  |
| resident | claude | c | setsid | TimedOut / Timeout: assignment deadline exceeded | 2007 | leaf-setsid,mid,provider,ticker | leaf-setsid,mid,provider,ticker | shutdown-ok; 2013; leaf-setsid; leaf-setsid | [harness killed 1 leftover] |
| resident | claude | c | stubborn | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 8006 | provider,ticker | provider | shutdown-error; 0; none; none |  |
| resident | claude | d | child | Cancelled / Cancelled: Turn interrupted | 1005 | child,provider,ticker | child,provider,ticker | shutdown-ok; 2014; none; none | interrupt() issued at 1 s returned after 3 ms |
| resident | claude | d | ignore | Cancelled / Cancelled: Turn interrupted | 1007 | child-ignore,provider,ticker | child-ignore,provider,ticker | shutdown-ok; 2014; none; none | interrupt() issued at 1 s returned after 5 ms |
| resident | claude | d | pipe | Cancelled / Cancelled: Turn interrupted | 1010 | child,provider,ticker | child,provider,ticker | shutdown-ok; 2013; none; none | interrupt() issued at 1 s returned after 8 ms |
| resident | claude | d | plain | Cancelled / Cancelled: Turn interrupted | 1009 | provider,ticker | provider,ticker | shutdown-ok; 2014; none; none | interrupt() issued at 1 s returned after 6 ms |
| resident | claude | d | setsid | Cancelled / Cancelled: Turn interrupted | 1005 | leaf-setsid,mid,provider,ticker | leaf-setsid,mid,provider,ticker | shutdown-ok; 2014; leaf-setsid; leaf-setsid | interrupt() issued at 1 s returned after 3 ms [harness killed 1 leftover] |
| resident | claude | d | stubborn | interrupt-error / Settlement: interruption is pending; settlement unconfirmed | 4004 | provider,ticker | provider,ticker | shutdown-error; 0; provider,ticker; none | interrupt() returned Err after 3002 ms |
| resident | claude | e | child | Failed / Protocol: native failure cleanup: duplex child exited unsuccessfully (signal: 9 ... | 2536 | none | none | shutdown-error; 0; none; none |  |
| resident | claude | e | ignore | Failed / Protocol: native failure cleanup: duplex child exited unsuccessfully (signal: 9 ... | 2539 | none | none | shutdown-error; 0; none; none |  |
| resident | claude | e | pipe | HANG | 30002 | child,provider(zombie) | child,provider(zombie) | shutdown-error; 2001; none; none | wait returned condition_met=false after 30 s; run state Running |
| resident | claude | e | plain | Failed / Protocol: native failure cleanup: duplex child exited unsuccessfully (signal: 9 ... | 2547 | none | none | shutdown-error; 0; none; none |  |
| resident | claude | e | setsid | Failed / Protocol: native failure cleanup: duplex child exited unsuccessfully (signal: 9 ... | 2540 | leaf-setsid | leaf-setsid | shutdown-error; 0; leaf-setsid; leaf-setsid | [harness killed 1 leftover] |
| resident | claude | ed | pipe | Failed / Protocol: native interruption did not settle: Ok(Err("io error: failed to write ... | 5003 | none | none | shutdown-error; 0; none; none |  |
| resident | claude | ed | plain | Failed / Protocol: native failure cleanup: duplex child exited unsuccessfully (signal: 9 ... | 2530 | none | none | shutdown-error; 0; none; none |  |
| resident | claude | f | child | HANG | 12002 | child,provider | child,provider | shutdown-error; 3002; child,provider; child,provider | wait returned condition_met=false after 12 s; run state Running [harness killed 2 leftover] |
| resident | claude | f | ignore | HANG | 12002 | child-ignore,provider | child-ignore,provider | shutdown-error; 3002; child-ignore,provider; child-ignore,provider | wait returned condition_met=false after 12 s; run state Running [harness killed 2 leftover] |
| resident | claude | f | pipe | HANG | 12002 | child,provider | child,provider | shutdown-error; 3001; child,provider; child,provider | wait returned condition_met=false after 12 s; run state Running [harness killed 2 leftover] |
| resident | claude | f | plain | HANG | 12002 | provider | provider | shutdown-error; 3002; provider; provider | wait returned condition_met=false after 12 s; run state Running [harness killed 1 leftover] |
| resident | claude | f | setsid | HANG | 12002 | leaf-setsid,mid,provider | leaf-setsid,mid,provider | shutdown-error; 3001; leaf-setsid,mid,provider; leaf-setsid,mid,provider | wait returned condition_met=false after 12 s; run state Running [harness killed 3 leftover] |
| resident | claude | f | stubborn | HANG | 12002 | provider | provider | shutdown-error; 3002; provider; provider | wait returned condition_met=false after 12 s; run state Running [harness killed 1 leftover] |
| resident | claude | fd | child | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9007 | child,provider | provider(zombie) | shutdown-error; 0; none; none |  |
| resident | claude | fd | ignore | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9006 | child-ignore,provider | child-ignore,provider(zombie) | shutdown-error; 0; none; none |  |
| resident | claude | fd | pipe | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9006 | child,provider | provider(zombie) | shutdown-error; 0; none; none |  |
| resident | claude | fd | plain | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9007 | provider | provider(zombie) | shutdown-error; 0; none; none |  |
| resident | claude | fd | setsid | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9006 | leaf-setsid,mid,provider | leaf-setsid,provider(zombie) | shutdown-error; 0; leaf-setsid; leaf-setsid | [harness killed 1 leftover] |
| resident | claude | fd | stubborn | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9005 | provider | provider | shutdown-error; 0; none; none |  |
| resident | claude | g | plain | HANG | 12001 | provider | provider | shutdown-ok; 2016; none; none | wait returned condition_met=false after 12 s; run state AwaitingInput |
| resident | claude | gd | plain | TimedOut / Timeout: assignment deadline exceeded | 3006 | provider | provider | shutdown-ok; 2013; none; none |  |
| resident | claude | gr | plain | Failed / Unsupported: native interaction rejected by this worker's interaction mode | 2041 | none | none | shutdown-ok; 0; none; none |  |
| resident | claude | h | plain | Failed / Protocol: native failure cleanup: duplex child exited unsuccessfully (exit statu... | 7024 | none | none | shutdown-error; 0; none; none |  |
| resident | claude | hd | plain | Failed / Protocol: native interruption did not settle: Ok(Err("duplex session is closed")... | 6004 | provider(zombie) | none | shutdown-error; 0; none; none |  |
| resident | claude | i | plain | HANG | 12002 | provider | provider | shutdown-ok; 2016; none; none | wait returned condition_met=false after 12 s; run state Running |
| resident | claude | id | plain | TimedOut / Timeout: assignment deadline exceeded | 3005 | provider | provider | shutdown-ok; 2014; none; none |  |
| resident | claude | j | child | none: no RunResult; run still owned by the engine | 1002 | child,provider,ticker | child,provider,ticker | shutdown-ok; 2017; none; none | wait future dropped at 1 s; run state Running |
| resident | claude | j | ignore | none: no RunResult; run still owned by the engine | 1002 | child-ignore,provider,ticker | child-ignore,provider,ticker | shutdown-ok; 2017; none; none | wait future dropped at 1 s; run state Running |
| resident | claude | j | pipe | none: no RunResult; run still owned by the engine | 1002 | child,provider,ticker | child,provider,ticker | shutdown-ok; 2015; none; none | wait future dropped at 1 s; run state Running |
| resident | claude | j | plain | none: no RunResult; run still owned by the engine | 1001 | provider,ticker | provider,ticker | shutdown-ok; 2017; none; none | wait future dropped at 1 s; run state Running |
| resident | claude | j | setsid | none: no RunResult; run still owned by the engine | 1002 | leaf-setsid,mid,provider,ticker | leaf-setsid,mid,provider,ticker | shutdown-ok; 2016; leaf-setsid; leaf-setsid | wait future dropped at 1 s; run state Running [harness killed 1 leftover] |
| resident | claude | j | stubborn | none: no RunResult; run still owned by the engine | 1002 | provider,ticker | provider,ticker | shutdown-error; 3002; provider,ticker; provider,ticker | wait future dropped at 1 s; run state Running [harness killed 2 leftover] |
| resident | claude | j2 | child | none: no RunResult; Engine dropped | 1002 | provider(zombie) | none | n/a | Engine dropped at 1 s |
| resident | claude | j2 | ignore | none: no RunResult; Engine dropped | 1002 | child-ignore,provider(zombie) | none | n/a | Engine dropped at 1 s |
| resident | claude | j2 | pipe | none: no RunResult; Engine dropped | 1002 | provider(zombie) | none | n/a | Engine dropped at 1 s |
| resident | claude | j2 | plain | none: no RunResult; Engine dropped | 1002 | provider(zombie) | none | n/a | Engine dropped at 1 s |
| resident | claude | j2 | setsid | none: no RunResult; Engine dropped | 1002 | leaf-setsid,provider(zombie) | leaf-setsid | n/a | Engine dropped at 1 s [harness killed 1 leftover] |
| resident | codex | a | plain | Succeeded | 29 | provider | provider | shutdown-ok; 0; none; none |  |
| resident | codex | b | plain | Failed / Protocol: native failure cleanup: codex command failed: /var/folders/kr/h7p0086n... | 24 | none | none | shutdown-error; 0; none; none |  |
| resident | codex | c | child | TimedOut / Timeout: assignment deadline exceeded | 2006 | child,provider,ticker | child,provider,ticker | shutdown-ok; 81; child; child | [harness killed 1 leftover] |
| resident | codex | c | ignore | TimedOut / Timeout: assignment deadline exceeded | 2010 | child-ignore,provider,ticker | child-ignore,provider,ticker | shutdown-ok; 122; child-ignore; child-ignore | [harness killed 1 leftover] |
| resident | codex | c | pipe | TimedOut / Timeout: assignment deadline exceeded | 2005 | child,provider,ticker | child,provider,ticker | shutdown-ok; 502; child; child | [harness killed 1 leftover] |
| resident | codex | c | plain | TimedOut / Timeout: assignment deadline exceeded | 2005 | provider,ticker | provider,ticker | shutdown-ok; 121; none; none |  |
| resident | codex | c | setsid | TimedOut / Timeout: assignment deadline exceeded | 2005 | leaf-setsid,mid,provider,ticker | leaf-setsid,mid,provider,ticker | shutdown-ok; 101; leaf-setsid,mid; leaf-setsid,mid | [harness killed 2 leftover] |
| resident | codex | c | stubborn | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 8006 | provider,ticker | provider | shutdown-error; 0; provider; none |  |
| resident | codex | d | child | Cancelled / Cancelled: Codex turn interrupted | 1005 | child,provider,ticker | child,provider,ticker | shutdown-ok; 74; child; child | interrupt() issued at 1 s returned after 2 ms [harness killed 1 leftover] |
| resident | codex | d | ignore | Cancelled / Cancelled: Codex turn interrupted | 1009 | child-ignore,provider,ticker | child-ignore,provider,ticker | shutdown-ok; 74; child-ignore; child-ignore | interrupt() issued at 1 s returned after 7 ms [harness killed 1 leftover] |
| resident | codex | d | pipe | Cancelled / Cancelled: Codex turn interrupted | 1006 | child,provider,ticker | child,provider,ticker | shutdown-ok; 502; child; child | interrupt() issued at 1 s returned after 3 ms [harness killed 1 leftover] |
| resident | codex | d | plain | Cancelled / Cancelled: Codex turn interrupted | 1009 | provider,ticker | provider,ticker | shutdown-ok; 79; none; none | interrupt() issued at 1 s returned after 7 ms |
| resident | codex | d | setsid | Cancelled / Cancelled: Codex turn interrupted | 1005 | leaf-setsid,mid,provider,ticker | leaf-setsid,mid,provider,ticker | shutdown-ok; 71; leaf-setsid,mid; leaf-setsid,mid | interrupt() issued at 1 s returned after 3 ms [harness killed 2 leftover] |
| resident | codex | d | stubborn | interrupt-error / Settlement: interruption is pending; settlement unconfirmed | 4004 | provider,ticker | provider | shutdown-error; 0; provider; none | interrupt() returned Err after 3001 ms |
| resident | codex | e | child | Failed / Protocol: native failure cleanup: codex command failed: /var/folders/kr/h7p0086n... | 528 | child | child | shutdown-error; 0; child; child | [harness killed 1 leftover] |
| resident | codex | e | ignore | Failed / Protocol: native failure cleanup: codex command failed: /var/folders/kr/h7p0086n... | 620 | child-ignore | child-ignore | shutdown-error; 0; child-ignore; child-ignore | [harness killed 1 leftover] |
| resident | codex | e | pipe | HANG | 30001 | child,provider(zombie) | child,provider(zombie) | shutdown-error; 3001; child; child | wait returned condition_met=false after 30 s; run state Running [harness killed 1 leftover] |
| resident | codex | e | plain | Failed / Protocol: native failure cleanup: codex command failed: /var/folders/kr/h7p0086n... | 534 | none | none | shutdown-error; 0; none; none |  |
| resident | codex | e | setsid | Failed / Protocol: native failure cleanup: codex command failed: /var/folders/kr/h7p0086n... | 533 | leaf-setsid,mid | leaf-setsid,mid | shutdown-error; 0; leaf-setsid,mid; leaf-setsid,mid | [harness killed 2 leftover] |
| resident | codex | ed | pipe | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Abn... | 6504 | child | child | shutdown-error; 0; child; child | [harness killed 1 leftover] |
| resident | codex | ed | plain | Failed / Protocol: native failure cleanup: codex command failed: /var/folders/kr/h7p0086n... | 665 | none | none | shutdown-error; 0; none; none |  |
| resident | codex | f | child | HANG | 12002 | child,provider | child,provider | shutdown-error; 3001; child,provider; provider(zombie) | wait returned condition_met=false after 12 s; run state Running |
| resident | codex | f | ignore | HANG | 12002 | child-ignore,provider | child-ignore,provider | shutdown-error; 3002; child-ignore,provider; child-ignore,provider(zombie) | wait returned condition_met=false after 12 s; run state Running [harness killed 1 leftover] |
| resident | codex | f | pipe | HANG | 12002 | child,provider | child,provider | shutdown-error; 3001; child,provider; provider(zombie) | wait returned condition_met=false after 12 s; run state Running |
| resident | codex | f | plain | HANG | 12002 | provider | provider | shutdown-error; 3002; provider; provider(zombie) | wait returned condition_met=false after 12 s; run state Running |
| resident | codex | f | setsid | HANG | 12002 | leaf-setsid,mid,provider | leaf-setsid,mid,provider | shutdown-error; 3002; leaf-setsid,mid,provider; leaf-setsid,provider(zombie) | wait returned condition_met=false after 12 s; run state Running [harness killed 1 leftover] |
| resident | codex | f | stubborn | HANG | 12002 | provider | provider | shutdown-error; 3002; provider; provider | wait returned condition_met=false after 12 s; run state Running [harness killed 1 leftover] |
| resident | codex | fd | child | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9006 | child,provider | provider(zombie) | shutdown-error; 0; provider(zombie); none |  |
| resident | codex | fd | ignore | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9006 | child-ignore,provider | child-ignore,provider(zombie) | shutdown-error; 0; child-ignore,provider(zombie); none |  |
| resident | codex | fd | pipe | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9006 | child,provider | provider(zombie) | shutdown-error; 0; provider(zombie); none |  |
| resident | codex | fd | plain | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9006 | provider | provider(zombie) | shutdown-error; 0; provider(zombie); none |  |
| resident | codex | fd | setsid | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9006 | leaf-setsid,mid,provider | leaf-setsid,provider(zombie) | shutdown-error; 0; leaf-setsid,provider(zombie); leaf-setsid | [harness killed 1 leftover] |
| resident | codex | fd | stubborn | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9006 | provider | provider | shutdown-error; 0; provider; none |  |
| resident | codex | g | plain | HANG | 12002 | provider | provider | shutdown-ok; 6; none; none | wait returned condition_met=false after 12 s; run state AwaitingInput |
| resident | codex | gd | plain | TimedOut / Timeout: assignment deadline exceeded | 3009 | provider | provider | shutdown-ok; 0; none; none |  |
| resident | codex | gr | plain | Failed / Unsupported: native interaction rejected by this worker's interaction mode | 33 | none | none | shutdown-ok; 0; none; none |  |
| resident | codex | h | plain | Failed / Protocol: Codex app-server exited | 10534 | none | none | shutdown-ok; 0; none; none |  |
| resident | codex | hd | plain | Failed / Protocol: native interruption did not settle: Err(Elapsed(())); cleanup: Err(Unc... | 9007 | provider(zombie) | none | shutdown-error; 0; none; none |  |
| resident | codex | i | plain | HANG | 12001 | provider | provider | shutdown-ok; 3; none; none | wait returned condition_met=false after 12 s; run state Running |
| resident | codex | id | plain | TimedOut / Timeout: assignment deadline exceeded | 3009 | provider | provider | shutdown-ok; 0; none; none |  |
| resident | codex | j | child | none: no RunResult; run still owned by the engine | 1002 | child,provider,ticker | child,provider,ticker | shutdown-ok; 92; child; child | wait future dropped at 1 s; run state Running [harness killed 1 leftover] |
| resident | codex | j | ignore | none: no RunResult; run still owned by the engine | 1002 | child-ignore,provider,ticker | child-ignore,provider,ticker | shutdown-ok; 107; child-ignore; child-ignore | wait future dropped at 1 s; run state Running [harness killed 1 leftover] |
| resident | codex | j | pipe | none: no RunResult; run still owned by the engine | 1002 | child,provider,ticker | child,provider,ticker | shutdown-ok; 506; child; child | wait future dropped at 1 s; run state Running [harness killed 1 leftover] |
| resident | codex | j | plain | none: no RunResult; run still owned by the engine | 1002 | provider,ticker | provider,ticker | shutdown-ok; 110; none; none | wait future dropped at 1 s; run state Running |
| resident | codex | j | setsid | none: no RunResult; run still owned by the engine | 1002 | leaf-setsid,mid,provider,ticker | leaf-setsid,mid,provider,ticker | shutdown-ok; 94; leaf-setsid,mid; leaf-setsid,mid | wait future dropped at 1 s; run state Running [harness killed 2 leftover] |
| resident | codex | j | stubborn | none: no RunResult; run still owned by the engine | 1002 | provider,ticker | provider,ticker | shutdown-error; 3001; provider,ticker; provider | wait future dropped at 1 s; run state Running [harness killed 1 leftover] |
| resident | codex | j2 | child | none: no RunResult; Engine dropped | 1002 | child | child | n/a | Engine dropped at 1 s [harness killed 1 leftover] |
| resident | codex | j2 | ignore | none: no RunResult; Engine dropped | 1002 | child-ignore | child-ignore | n/a | Engine dropped at 1 s [harness killed 1 leftover] |
| resident | codex | j2 | pipe | none: no RunResult; Engine dropped | 1002 | child | child | n/a | Engine dropped at 1 s [harness killed 1 leftover] |
| resident | codex | j2 | plain | none: no RunResult; Engine dropped | 1002 | none | none | n/a | Engine dropped at 1 s |
| resident | codex | j2 | setsid | none: no RunResult; Engine dropped | 1002 | leaf-setsid,mid | leaf-setsid,mid | n/a | Engine dropped at 1 s [harness killed 2 leftover] |

## Measured: gaps against criterion 1

Each gap names the rows that show it. "Both providers" means the same result on
the Claude and Codex rows.

1. **No deadline means no outcome.** One-shot `f` (silent hang) and `h`
   (stdout closed, process alive) with no timeout never return: HANG at 12 s,
   both providers: `f` in all six variants, `h` buffered and streaming. Resident
   `f` and `i` (output ends, no final result) with no assignment timeout stay
   `Running`, and `g` (permission asked, nobody answers) stays `AwaitingInput`;
   `Engine::wait` returns `condition_met=false` only because the harness gave it
   a 12 s timeout. due has no default deadline. With a deadline the same cases
   end: one-shot `fd`, `hd` return TimedOut at 5.0 s for a 3 s timeout (3 s plus
   the 2 s grace); resident `id` and `gd` return TimedOut at about 3.0 s.
2. **A silent provider with a deadline ends as `Failed`, not `TimedOut`, and
   late.** Resident `fd` (both providers, every variant): Failed/Protocol at
   about 9.0 s for a 3 s timeout, message "native interruption did not settle
   ... cleanup: Err(Unconfirmed(...))". The engine sent an interrupt, waited
   out the control timeout, then closed the worker. Resident `hd` ends Failed
   at 6.0 s (Claude) and 9.0 s (Codex), and resident `h` ends Failed at 7.0 s
   (Claude) and 10.5 s (Codex).
3. **A crashed provider whose descendant holds its output pipe hangs the
   call.** `e` with variant `pipe`: one-shot buffered, both providers, HANG at
   30 s; resident, both providers, HANG at 30 s with the provider reported as a
   zombie and the run state `Running`. With a 3 s timeout (`ed` `pipe`) the
   one-shot call ends TimedOut at 5.0 s (not as a crash) and the resident call
   ends Failed/Protocol at 5.0 s (Claude) and 6.5 s (Codex).
4. **A provider that ignores interrupt leaves the caller without a result.**
   Resident `d` `stubborn`, both providers: `Engine::interrupt` returns
   `Err(Settlement, "interruption is pending; settlement unconfirmed")` at 4.0
   s (interrupt at 1 s plus the 3 s settlement bound), not a `RunResult`. The
   run stays `Running` until later steps finish it as Failed/Protocol. Resident
   `c` `stubborn` returns Failed/Protocol at 8.0 s, not TimedOut. Resident
   `f` and `j` `stubborn`: `Engine::shutdown` returns
   `Err(Settlement, "shutdown requested; run ... has not settled")` after 3.0 s
   with the run state still `Running`.
5. **Dropping the future gives no outcome.** One-shot `j` (task aborted at
   1 s): the caller receives a `JoinError`, no `RunResult`. Resident `j`
   (`wait` dropped): the run continues under the engine; its state is `Running`
   when inspected immediately. Resident `j2` (`Engine` dropped): no result is
   obtainable. This is by construction, listed so the contract states it.
6. **Crash detail is lost on one-shot.** One-shot `e`, both providers, buffered
   and streaming: `Failed/Provider`, exit code -1, "exited with status -1".
   The signal is not named. Resident Claude names it ("signal: 9 (SIGKILL)");
   resident Codex reports "exit code -1".
7. **Refused has no status.** Resident `gr` returns `Failed/Unsupported`,
   "native interaction rejected by this worker's interaction mode", in 33 ms
   (Codex) and 2.0 s (Claude). One-shot `g`: Claude returns Succeeded with one
   permission denial counted (the fake reports one in the result); the Codex
   fake's declined command is not surfaced at all and the result is a plain
   Succeeded (this row depends on how faithful the fake is).
8. **Cancel and timeout always cost the grace period.** One-shot `c` returns at
   4.0 s for a 2 s timeout and `d` at 3.0 s for an interrupt at 1 s, for every
   variant. In a separate check with a throwaway script (not part of this crate),
   the fake provider exited 19 ms after SIGINT, so the 2 s is the wrapper's
   unconditional sleep.

## Measured: gaps against criterion 2

1. **Descendants in their own session survive every due cleanup.** Variant
   `setsid`, `leaf-setsid` alive at 1 s and 5 s (and `mid` where its group was
   not signalled): one-shot `c`, `d`, `f`, `fd`, `j` and `e`, both providers;
   resident `c`, `d`, `j`, `fd`, `f`, `e`, `j2` and, after `shutdown()`, `c`, `d`,
   `j`, `fd`, `f` (Claude: `leaf-setsid`; Codex: `leaf-setsid,mid`). The result carries no
   mention of them. The harness killed them at the end of each row.
2. **A provider that exits or crashes leaves its same-group children.** One-shot
   `e` with `child`, `ignore` and `setsid`, both providers: `child`,
   `child-ignore`, `leaf-setsid,mid` alive at 1 s and 5 s. The wrapper signals
   the group only on timeout, cancel and drop, not after the provider has
   already exited. Resident Codex `e` shows the same. Resident Claude `e`
   `child` and `ignore` are clean (the group is killed), but `setsid` leaves
   `leaf-setsid`.
3. **The resident worker outlives its run.** Resident `a`, `c`, `d`, `g` (with
   a deadline), `i` (with a deadline) and `j`, both providers: `provider` alive
   at 1 s and 5 s after the result, and gone only after `Engine::shutdown()`.
   This follows from the design (long-lived sessions) and the result says
   nothing about it.
4. **Resident Codex does not kill same-group children on a graceful close.**
   After `shutdown()` the `child`, `child-ignore` and `mid`/`leaf-setsid`
   processes of resident Codex rows `c`, `d`, `j` remain, and after
   `Engine` drop (`j2`) `child`, `child-ignore`, `mid`, `leaf-setsid` remain
   (Codex `j2` `plain` is clean). The fake exits on stdin EOF without touching
   its children; due's documentation says a real Codex stops its own commands
   on EOF, so this row shows what due does when the provider does not.
   Resident Claude kills the group on shutdown (`child`, `child-ignore` gone),
   and on `Engine` drop (gone by 5 s; the provider is a zombie at 1 s).
5. **Failed cleanup is reported without saying what remains.** Rows with
   "settlement unconfirmed" or "Unconfirmed" in the result (resident `c`
   `stubborn`, `d` `stubborn`, `fd`, `f`, `j` `stubborn`) give no pid, role or
   process list. In resident Claude `f` (all variants) and `j` `stubborn`, the
   provider was still alive 1 s and 5 s after `shutdown()` returned its error,
   and the harness killed it.
6. **Zombies.** Resident `fd` and `hd` (both providers) show the provider as a
   zombie at 5 s after the result; the later `shutdown()` stage shows none.

## Measured: what meets the criteria

One-shot, both providers (the rows match between Claude and Codex):

- Meets both: `a`, `b`, `i`, `g`; `c`, `d` for `plain`, `child`, `ignore`,
  `pipe`, `stubborn`; `fd` for `plain`, `child`, `ignore`, `pipe`, `stubborn`; `hd`, `id`; `ed` (both variants run); `e` `plain`;
  `j` for every variant except `setsid` (the group is killed on drop, without
  the grace period, and no result exists).
- Fails criterion 1 (no outcome): `f` in every variant, `h`, `e` `pipe`.
- Fails criterion 2: every `setsid` row, and `e` with `child`, `ignore`,
  `setsid`.
- The streaming path matches the buffered path on every row it ran, including
  `h` (HANG) and `e`.

Resident Claude and resident Codex:

- Meets criterion 1: `a`, `b`, `c` (not `stubborn`), `d` (not `stubborn`), `e`
  (not `pipe`), `ed`, `gd`, `gr`, `h`, `hd`, `id`; `fd` ends in a stated
  outcome but as Failed/Protocol at 9 s.
- Fails criterion 1: `f`, `g`, `i` without a deadline; `e` `pipe`; `d`
  `stubborn` (an `Err`, no result).
- Meets criterion 2 after `shutdown()` with no survivors: `a`, `b`, `h`, `gd`,
  `gr`, `id`, and the `plain` rows of `c`, `d`, `j`. Fails after `shutdown()`:
  every `setsid` row; Codex `c`, `d`, `j`, `e` and `ed` rows that have a descendant; the `f` rows.
- Provider alive after the run result in every case where the engine keeps the
  worker (see gap 3).

## Cases not exercised

- Real Claude Code and Codex. The fakes follow due's own fixtures and the
  wrapper crates' envelopes; nothing here shows how the real CLIs behave when
  signalled, when their stdin closes, or how they place tool processes in
  process groups. Real tool processes usually have their own process groups,
  which resembles the `setsid` variant more than the `child` variant.
- Linux. Only macOS was run. The wrappers use `prctl(PR_SET_PDEATHSIG)` on
  Linux only, so Linux rows could differ.
- The CLI process exiting while a provider is running. The harness keeps the
  calling process alive to measure after the call returns.
- Resident follow-up, resume and fork. Only the first assignment on a fresh
  worker was run.
- The one-shot permission case against a real approval flow. `claude -p` and
  `codex exec` have no live prompt, so the fakes model the automatic denial.
- The `settlement_timeout` default of 90 s. The harness used 3 s.
- One-shot streaming for every variant; streaming ran for the `plain` variant
  of `a`, `b`, `c`, `d`, `e`, `fd`, `h`, `i`, `j` only.
- Output volume. The fakes emit a few short lines; the wrappers' output limits
  were not exercised.
- Counterpart was not run (read-only, per the task).

## Read from code: what counterpart does for each gap

Read from `crates/counterpart-core/src/runner/process.rs`, `runner/turn.rs`,
`process_tree.rs` and the tests `crates/counterpart/tests/hardening.rs` and
`lifecycle.rs`. None of this was run, so it says what the code and its tests
claim, not what was measured here.

- **No deadline.** A worker's `timeout_ms` defaults to 600000 and is accepted
  between 1000 and 3600000 (`worker_input.rs`). When it fires the run is
  stopped and recorded as `turn_timeout`, "Turn exceeded timeout"
  (`supervisor/core_events.rs`). A hang therefore ends within ten minutes by
  default.
- **Same-group children and a provider that exits on its own.** The turn loop
  begins cleanup when the leader exits: it sends SIGTERM to the group at that
  point (`runner/turn.rs`, the `exit_rx` arm) and the cleanup loop then keeps
  signalling until the group has no live member and no tracked descendant is
  alive. A natural exit with children is covered by
  `successful_result_does_not_hide_natural_nonzero_or_unrequested_signal`.
- **Descendants in their own session (`setsid`).** The provider is started in
  its own session (`pre_exec` calls `setsid`, so the group never includes the
  caller). A capture task lists processes every 500 ms (`CAPTURE_INTERVAL`) and
  once more before stop (`before_stop`, bounded to 25 ms). `ProcessTree` keeps
  each descendant's pid and start time, adopts any process whose parent is
  already owned, and signals known descendants by identity (SIGTERM, then
  SIGKILL after the grace) until none is alive (`process_tree.rs`
  `adopt`, `signal`, `cleanup_state`). The tests
  `descendants_are_gone_when_interrupt_returns` (modes `descendant` and
  `detached`) and `panic_after_output_keeps_owner_until_detached_child_cleanup`
  cover this. Limit: a descendant created and detached between two captures is
  never seen. The code says so: `unobserved_descendants_possible: true`, and
  the warning "Pre-stop process capture exceeded 25 ms" in
  `unobserved_detached_child_has_explicit_capture_gap_warning`.
- **Pipe held by a surviving descendant after the provider is gone.** After
  SIGKILL the turn loop sets a `drain_bound` deadline, then closes the pipes and
  records `PIPE_WARNING`. `escaped_pipe_holder_drain_is_bounded` asserts the
  call finishes in under 4 s with tracking switched off.
- **Provider ignores SIGTERM or interrupt.** The group gets SIGKILL after
  `kill_grace`, and SIGKILL repeats every 100 ms, with tracked descendants
  signalled every 25 ms, until `cleanup_state` shows nothing left. When
  `stop_deadline` passes, a `CleanupFailed` observation carries warnings and
  process observations, cleanup continues, and the slot is held until it
  settles (`cleanup_failure_is_reported_then_settles_once`,
  `cleanup_inspection_panic_keeps_slot_until_inspector_recovers`).
- **Settlement diagnostics.** A settlement records the signals sent with
  timestamps, `descendants_after_exit`, `post_exit_group_signal`, warnings,
  `signal_error`, and per-process observations stating the scope of the claim:
  "observed process group and retained PID/start identities; never-observed
  descendants are not covered" (`process_observation` in `process.rs`). This is
  the "says what could not be confirmed" form criterion 2 asks for, though it
  names no pids of survivors.
- **Dropping the caller.** `Drop for OwnedProcess` sends SIGKILL to the group
  while the leader is unreaped and states that this is best effort, not a
  settlement. Tracked descendants outside the group are not signalled on that
  path.
- **Worker outliving its run.** The runner owns "one active subprocess family
  per turn" (`TurnOwner::spawn`) and settles it before reporting, so no idle
  resident worker exists. The cost is that there are no long-lived native
  sessions or live permission requests in that runner.
- **Stdout closed while the provider stays alive (`h`).** The turn loop treats
  stdout EOF as a closed pipe and keeps waiting for the leader to exit, so by
  the code it ends at the turn timeout, not earlier.

## Judgment

This section is opinion drawn from the measurements above.

**Recommendation: keep counterpart's runner as the process layer, under due's
request and result contracts, and do not adopt due's engine as the core for
criteria 1 and 2.** Reasons:

1. Every criterion 2 gap and the pipe-holder hang are in code that due-core
   does not own. Spawning, signalling and reaping happen inside
   `claude-wrapper` and `codex-wrapper`; due-core passes a binary path and
   options and receives results. Hardening due's engine would mean changing two
   wrapper crates, or wrapping the provider in a supervisor of our own, plus
   changing the engine's settlement logic. Counterpart's runner already owns its
   process lifecycle and has tests for descendants, forced escalation, bounded
   drains and unconfirmed cleanup. Those claims are unmeasured here.
2. The resident engine's design keeps the provider alive after the run, and the
   result does not say so. That conflicts with criterion 2 as the README words
   it ("after any outcome"), unless the caller always shuts the worker down,
   and even then Codex rows and `setsid` rows leave processes behind and failed
   shutdowns leave the run `Running`.
3. The measured gaps are not edge cases: the no-deadline hang, the setsid
   survivor and the crash-with-children survivor reproduce on every row of
   both providers on both paths.
4. What due has that counterpart's runner lacks is long-lived native sessions
   with live permission requests, a CLI `run`, and versioned result schemas.
   None of it bears on criteria 1 and 2, so it can be layered on later.

**What I am unsure of.**

- Counterpart has not been measured. The code reading says it handles each gap,
  but its tests use their own fixtures. The same harness (fake providers, pid
  ledger, survivor scan) could drive counterpart's runner and turn the code
  reading into numbers. Doing that before committing to the recommendation is
  the cheapest way to check it.
- Whether the real CLIs put tool processes in their own groups, and how they
  react to EOF and SIGINT, decides how much of the `setsid` and Codex-EOF
  findings matter in practice.
- A middle path is possible and not tested: keep due's wrappers and engine but
  launch each provider through a small supervisor binary (due accepts a binary
  path) that starts the provider in its own session and tracks descendants as
  counterpart does. It would not help when the wrapper SIGKILLs the supervisor
  itself, so it is a partial fix at best.
- The resident `shutdown()` failures in rows `f` depend on the 3 s settlement
  bound the harness set; with due's 90 s default the same rows would take far
  longer to report and might settle.

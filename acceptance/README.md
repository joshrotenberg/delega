# delega acceptance harness

`delega-acceptance` measures the first two acceptance criteria of the
repository README against the built `delega` binary:

1. Every run ends in exactly one stated outcome. Never a hang and never silence.
2. After any outcome, the provider process and its descendants are gone, or the
   result says cleanup could not be confirmed and what remains.

It drives `delega run --output json` as a black box, for both providers, over a
matrix of fake-provider behaviours crossed with caller actions, and records for
each row what was reported and what was really left running. It adapts the
matrix of the outcome spike (`delega-dot-spike-outcomes`, commit `7f4da23`),
which drove counterpart's and due's libraries, so the results can be compared:
[COMPARISON.md](https://github.com/joshrotenberg/delega/blob/main/acceptance/COMPARISON.md).

Results: [RESULTS.md](https://github.com/joshrotenberg/delega/blob/main/acceptance/RESULTS.md) (summary and expected failures),
`results.jsonl` (one row per case), `repeats.jsonl` (three more full runs, for
variation).

## Run

From the repository root:

```sh
cargo run --release -p delega-acceptance -- --all          # the whole matrix, about 75 s
cargo run --release -p delega-acceptance -- --all --repeat 3 --out /tmp/repeats.jsonl
cargo run -p delega-acceptance -- --smoke                  # the rows `cargo test` runs
cargo run -p delega-acceptance -- --filter codex-d-       # rows whose id contains the text
cargo run --release -p delega-acceptance -- --timing-gaps --repeat 3 --out /tmp/gaps.jsonl  # only the timing-dependent gap rows
cargo run -p delega-acceptance -- --list                   # row ids
cargo run -p delega-acceptance -- --report                 # regenerate RESULTS.md and COMPARISON.md
cargo run -p delega-acceptance -- --sweep                  # kill leftovers of an interrupted harness run
```

`--timing-gaps` selects the rows whose expectation is a timing-dependent known
gap, by the matrix flag and not by name. It combines with `--repeat` and narrows
`--all`, `--smoke` and `--filter`; with no other selector it starts from the
whole matrix. If it selects no row the harness exits 2, like a filter that
matches nothing.

`--all` writes `acceptance/results.jsonl` and `results.meta.json`; use `--out`
to write elsewhere. `--jobs N` sets how many cases run at once (default 8) and
`--repeat N` runs the whole matrix N times, to expose flakiness. Use `--release`
for timings: the harness builds `delega` with the profile it was built with.
`--delega PATH` measures a binary that is already built.

The run exits non-zero when a row that is expected to pass fails, when a
known-gap row fails in any way other than a missing survivor, when a known gap
is closed (`gap_closed`, below), or when a process of the run is alive at the
end. After a run the harness kills what is
left and prints how many processes carried its marker; `ps` shows none.

`cargo test --workspace` runs `tests/smoke.rs`, which runs eleven rows (both
providers; a normal run, a normal run with a child that ignores TERM, an error, a
timeout with a child, a signal, a crash, output with no final answer, a closed
stdout, a run with process listing unavailable that must report cleanup as not
confirmed, and a SIGKILL of `delega` whose worker the harness must clean up) in
about ten seconds. It never runs the matrix. A skipped row fails the run unless
`--allow-skips` is given.

After a new full run, regenerate the documents:

```sh
cargo run --release -p delega-acceptance -- --all
cargo run --release -p delega-acceptance -- --all --repeat 3 --out acceptance/repeats.jsonl
cargo run -p delega-acceptance -- --report
```

`--report` reads `results.jsonl`, `results.meta.json`, `repeats.jsonl` and
`spike-numbers.jsonl` and rewrites `RESULTS.md` and `COMPARISON.md`.

`spike-numbers.jsonl` holds the numbers copied from the spike's committed
results; `scripts/extract-spike-numbers.py` produced it.

`tests/committed_results.rs`, part of `cargo test --workspace`, keeps the
committed results (`results.jsonl`, `repeats.jsonl`) and the documents
generated from them (`RESULTS.md`, `COMPARISON.md`) current. It fails when the committed results do not hold exactly the
matrix's rows (each row once in `results.jsonl`, as repeats 1 to 3 in
`repeats.jsonl`), when any expectation a committed row records (outcome, exit,
cleanup verdict, survivors, leak class, timing dependence, reason text) differs
from `src/matrix.rs`, or when `RESULTS.md` or `COMPARISON.md` differs from what
`--report` generates. What a change costs to bring them back in step:

- A change to the report format only: `--report`, a few seconds.
- A change to the matrix (a row, an expectation or a reason text): the three
  commands above on a quiet machine, about 80 s for `--all` and 240 s for the
  repeats on an Apple M4 Pro (2026-11-19), then `--report`.

Either way the regenerated files go in the same pull request as the change.

## Retries after a late process listing

`delega` waits at most 500 ms (a requested stop) or 3.1 s (after the worker
exits) for the process listing it takes before its first signal. On a machine
where `ps` is slower than that, it reports cleanup as `Unconfirmed` with the
reason `delega_core::process::LATE_LISTING_REASON`. A row that expects a
confirmed cleanup is then run again, up to five runs in all, but only when that
is the whole failure: the cleanup verdict is exactly the late-listing verdict
(`delega_core::process::is_late_listing_only`: that reason alone, or repeated
for the version probe and the run, with no pids and no process groups), and the
only problem `judge` found is the confirmed mismatch, so the exit status, the
outcome and the survivors were as expected. Any other failure is reported as
measured. Each result records `attempts` and the `discarded` reasons in the
JSONL output, and the summary line says how many rows needed a retry and how
many runs the matrix took in total. A row whose listing is late in all five runs
fails.

## Safety

Everything runs against fake provider scripts. Each case gets a temporary
directory with its own `HOME`, the fakes as `DELEGA_CLAUDE_BIN` and
`DELEGA_CODEX_BIN`, and a cleared environment (`PATH=/usr/bin:/bin` and the few
variables set by the harness). Nothing reads real provider state, a shell
profile or the network, and no real `claude` or `codex` is started. `delega`
runs in a process group of its own, so no signal the harness sends reaches the
harness. The harness kills every process it finds by marker after each case,
including the cases that are designed to leave strays, and sweeps again at the
end.

## The matrix

A row id is `<provider>-<case>-<variant>`, for example `codex-d-setsid`. A case
is a provider behaviour and a caller action; a variant is what the provider
leaves running beside itself. Case letters and variant names of the spike are
kept where a row carries over.

| Case | Provider behaviour | Caller action | Expected |
| --- | --- | --- | --- |
| `a` | finishes normally | wait | `completed`, exit 0 |
| `b` | exits 3 with an error | wait | `failed`, exit 1 |
| `c` | output every 0.2 s, never finishes | `--timeout 2s` | `timed_out`, exit 124 |
| `d` | as `c`, no timeout | SIGINT at 1 s | `interrupted`, exit 130 |
| `dt` | as `d` | SIGTERM at 1 s | `interrupted`, exit 130 |
| `dh` | as `d` | SIGHUP at 1 s | `interrupted`, exit 130 |
| `e` | SIGKILLs itself 0.5 s in (1.5 s with the `bare` variant) | wait | `failed`, exit 1 |
| `ed` | as `e` | `--timeout 25s` | `failed`, exit 1 |
| `fd` | hangs, no output | `--timeout 3s` | `timed_out`, exit 124 |
| `hd` | closes stdout, stays alive | `--timeout 3s` | `timed_out`, exit 124 |
| `i` | output ends with no final answer, exit 0 | wait | `failed`, exit 1 |
| `id` | as `i` | `--timeout 3s` | `failed`, exit 1 |
| `fl` | output at full speed, never finishes | `--timeout 3s` | `timed_out`, exit 124 |
| `fli` | as `fl`, no timeout | SIGINT at 1 s | `interrupted`, exit 130 |
| `dc` | as `c` | reader closes stdout at 1 s | exit 124, nothing to read |
| `dca` | finishes after 1.5 s | reader closes stdout at 0.3 s | exit 0, nothing to read |
| `k` | hangs, no output | SIGKILL of `delega` at 1 s | killed by signal 9, no output |
| `u` | finishes normally | wait, process listing unavailable | `completed`, exit 0, cleanup **not** confirmed |
| `ud` | hangs, no output | `--timeout 3s`, process listing unavailable | `timed_out`, exit 124, cleanup not confirmed |
| `ui` | as `c`, no timeout | SIGINT at 1 s, process listing unavailable | `interrupted`, exit 130, cleanup not confirmed |

Variants: `plain` (nothing), `child` (one child in the provider's group),
`setsid` (a child in the group that starts a grandchild in a session of its
own), `ignore` (a child that ignores TERM and INT), `stubborn` (the provider
ignores TERM and INT itself), `pipe` (a child that keeps the provider's stdio),
`orphan` (a child that starts a `setsid` grandchild and exits at once; these rows are timing-dependent),
`orphan-late` (as `orphan`, after 1.3 s), `relay` (a fork relay: each process
ignores TERM, leads a group of its own, lives a few milliseconds and forks its
successor), `bare` (a `sleep 300` in a session of its own whose command line
does not carry the marker, found through the fake's pid ledger instead). With `e` the provider waits 1.5 s after the `sleep` has left the session before it crashes, so several listings (`delega` lists at spawn, at 50, 100 and 200 ms, then every 500 ms) have run while the provider is its parent; the shorter 0.5 s lost that race under heavy load (delega issue 79). With `a` the provider waits 0.3 s and then, best effort, polling for about 3 s at most, until one of `delega`'s listings (a `ps` child of `delega`) has just ended, before it starts the `sleep` and ends at once, so the child appears between two listings; started at once, the listing at 50 or 100 ms adopts it, and after a fixed delay alone a slow runner ran into the first periodic listing (delega issues 110 and 112). With `u`, where listing is turned off, it waits the 0.3 s only. `orphan` starts its child 0.3 s after the provider starts. Each case runs the variants listed for it in `src/matrix.rs`; the
whole matrix is 188 rows.

Actions that start at 1 s wait until the fake has recorded every process the
variant starts, so the action lands with the descendants running.

`ed` is about a crash that happens while a time limit is pending, not about the
limit, so its limit is 25 s, far longer than the 0.5 s to the crash. With a 3 s
limit a slow start on a loaded machine let the limit fire first and the row
exited 124. The row checks that a crash while a limit is pending is reported as
`failed`, not `timed_out`. It does not test the limit: once the provider has
exited the limit is disarmed, so a `delega` that waited for the pipe of
`ed-pipe` to close would hit the harness's 30 s hang limit with either limit.

The fakes wait for events, not for time, before they let the provider go on.
The `relay` variant waits for the heartbeat file, `bare` for a marker file the
process writes after it has left its session, `orphan` until the grandchild has
started its own session and its parent is no longer its parent, and the other
variants for their ledger entries. Each wait has a deadline of 60 s that only a
broken machine reaches. A row therefore sets up the same situation on a slow
machine and a fast one.

### Spike rows not carried over

| Spike case | Why |
| --- | --- |
| `f` | Hang with no output and no deadline. `delega run` defaults to a 600 s limit, so the row would take ten minutes. `fd`, `d` and `k` cover the hang. |
| `h` | Closes stdout, stays alive, no deadline. Same 600 s default; `hd` covers it. |
| `g`, `gd`, `gr` | A permission asked and nobody answers. `delega run` answers every request no at once; there is no live request to leave unanswered. |
| `j` | The `wait` future is dropped. The CLI has no future; `dc` and `k` are the nearest. |
| `j2` | The handle is dropped. The CLI has no handle; `k` is the nearest. |

Cases `dt`, `dh`, `fl`, `fli`, `dc`, `dca` and `k`, the `relay` variant, and `a`
with every variant are new.

## Fake providers

The harness reuses the stream-accurate fakes the unit tests use
(`crates/delega-core/src/provider/tests/fakes/fake-claude.sh` and
`fake-codex.sh`). They gained one mode each, `acc:<behaviour>:<variant>`, which
sources `acc-lib.sh` and starts descendants through `acc-child.sh`. No existing
mode changed, and no unit test uses the new ones. The prompt of a case is the
mode, because the fakes read their behaviour from the first line of standard
input.

Descendants start through `perl` (`setsid` and default signal handling), so rows
whose variant needs it are skipped, visibly, when `perl` is missing from
`/usr/bin:/bin`.

## Measuring what is left

The harness copies the fakes and their helper scripts into a directory whose
name is unique to the case, `accmk-<run>-<n>`. Every process the fakes start
runs a script from that directory, so the name is in its command line. After
`delega` exits the harness looks in the process table (`ps -axww`) for the
marker, whatever `delega` reported. The marker it matches ends in `/`, so case 1
is not found in the command line of case 10 (a unit test covers it). It samples
at exit and again 1 s later, records the second set as the survivors, then kills
everything, stops the relay through a file it checks, and checks the table until
it is empty.

**What this can miss.** Detection reads the marker from a process's arguments as
`ps` shows them. A process that rewrites its own arguments, runs a program whose
path is outside the case directory with no marker in its arguments, or is started
by another user is not found by that. Two other means are used. The relay is also
detected by a heartbeat file it touches, because a relay process lives too briefly
to be listed reliably. The `bare` variant is a `sleep` in a session of its own with
no marker in its command line; the fake records its pid in a ledger, and the
harness counts it when that pid is alive and still runs `sleep`. A session-id
sweep of the worker's session is not used, because the point of these rows is
descendants that left the session. A process that is neither marked, recorded nor
touching a heartbeat is invisible to the harness.

Per row the results file records the provider, behaviour, action, exit status,
the number of JSON documents on standard output and whether the one validates
as a `RunResult`, the outcome, `cleanup.confirmed` with its reason, pids and
groups, the survivors, whether the report was honest (survivors exist only when
cleanup is not confirmed), the latency from the action to the exit, and the
expectation and verdict.

## Forcing an unconfirmed cleanup

Criterion 2 allows survivors when the result says cleanup could not be
confirmed. Rows `u`, `ud` and `ui` make that happen: they set
`DELEGA_TEST_NO_PROCESS_LISTING` in the environment of `delega`. This is a
test-only seam in `crates/delega-core/src/process/tree.rs`: when the variable is
set, whatever its value, no program is used to list processes. It cannot
substitute a program, so it can only make the verdict weaker. `delega` hard-codes
`/bin/ps` and `/usr/bin/ps`, and neither can be hidden without root, so no
arrangement from outside was found; nothing outside this harness sets the
variable. With no listing, the run must still end in its outcome, must report
`cleanup.confirmed` false, with a reason that contains "process inspection was
unavailable" and says what could not be checked. A descendant in a session of its
own then survives, as the report says.

A second way to an unconfirmed cleanup, a descendant that is seen and cannot be
killed within the cleanup deadline, was not built: without root there is no
process that survives SIGKILL on demand (an uninterruptible wait cannot be
arranged from a script, and a respawning process would need to outrun the
kill, which risks the machine). No row covers it.

Every row that has a result also carries an expectation on `cleanup.confirmed`
(true unless listed above), so a `delega` that reported confirmed unconditionally
fails the run.

## Expectations

`src/matrix.rs` holds them: per case the expected outcome, exit status, standard
output and `cleanup.confirmed`, and a list of expected failures (`GAPS`), each
with a class, a reason and exactly which roles may survive:

- **documented gap**: the process module's docs list it as known (a descendant
  that calls `setsid` and whose parent exited before any listing saw it, which
  is the `orphan` and `bare` rows; a fast fork relay; SIGKILL of `delega`).
- **undocumented gap**: a failure this harness found that the docs do not
  describe. None is listed now.
- **defect**: a failure that is neither. None is listed now.

A row's verdict is `pass`, `expected_failure`, `gap_not_shown`, `gap_closed`,
`unexpected_pass` or `fail`. A wrong outcome, exit status, `cleanup.confirmed`,
document or hang is always `fail`. A row with no gap passes only with no
survivor. A known-gap row says a survivor of the listed roles may remain:

- **`expected_failure`**: the listed roles survived, so the gap showed.
- **`gap_not_shown`**: nothing survived, on a row whose gap depends on timing
  (`relay`, `orphan` and `bare`, 22 runs of the matrix). Whether `delega` catches a process
  that left the worker's session before any listing saw it depends on timing, so
  on a busy machine a gap that showed on one run may not on the next. This is
  not a failure and not counted as an unexpected pass. The summary says how many
  known-gap runs did not show their gap and lists them.
- **`gap_closed`**: the gap is probably fixed and the table is out of date, so
  the run fails. Two rules produce it, both only for timing-dependent rows:
  with `--repeat N` and N at least 3, a row that was `gap_not_shown` in every
  one of the N runs; and in any single run, when there are at least 3
  timing-dependent gap runs and every one of them is `gap_not_shown` (all of
  them failing to show at once is not load noise). CI runs `--all` once, which
  applies the second rule, and then the timing-dependent gap rows with
  `--repeat 3`, which applies the first:

  ```sh
  cargo run --release --locked -p delega-acceptance -- \
    --timing-gaps --repeat 3 --out acceptance-gaps.jsonl
  ```

  `--timing-gaps` selects by the matrix flag, so a renamed variant stays in the
  step and the strict rows (`orphan-late`, the rows whose process listing is
  unavailable, and the `c`, `d`, `e` and `fd` bare rows) are not repeated. It
  selects exactly the 22 timing-dependent gap rows. Locally the repeat takes
  about a minute. A skipped timing-dependent row is never marked `gap_closed`.
- **`unexpected_pass`**: a known-gap row whose gap does not depend on timing
  (the SIGKILL `k` rows: after `delega` is killed nothing can stop the worker,
  so the provider always survives) had no survivor. That is not load noise and
  fails the run: either the gap is fixed and the table is out of date, or
  `delega` stopped before it started the worker.
- **`fail`**: anything else about the survivors: a role the row does not list, a
  subset of the listed roles, or an `unrecorded` survivor (a process that carries
  the case marker and that the ledger does not know, which is a real finding). An
  expected failure cannot hide a second leak.

Every process a fake starts appends its role to the ledger as its first act.
That includes each generation of the `relay` variant (role `relay-heartbeat`),
so a relay process that is sampled is attributed to its role. A marked process
the ledger does not know yet is given two seconds to record itself, which only
matters on a machine busy enough to delay a forked process that long, before it
is reported as `unrecorded`. A zombie counts as exited for that wait.

`delega`'s behaviour is never changed to make a row pass.

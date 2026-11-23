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

It also has a live suite for criterion 6, which runs a few cases per provider
against the installed `claude` and `codex` and only with `--live`; see
[Live suite](#live-suite).

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
cargo run -p delega-acceptance -- --live                   # the live suite, real providers (see below)
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

For selections that include Claude, the release harness checks for the system
`managed-mcp.json` before running any case. It checks only filesystem metadata,
including whether a symlink is present. If an entry exists or absence cannot be
confirmed, the harness stops. Use the debug harness (omit `--release` and
`--delega`) to build a debug binary and redirect the system root to a temporary
directory. The harness also applies the metadata check to a supplied `--delega`
binary because its build profile is unknown.

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

Everything but `--live` runs against fake provider scripts. Each case gets a temporary
directory with its own `HOME`, the fakes as `DELEGA_CLAUDE_BIN` and
`DELEGA_CODEX_BIN`, and a cleared environment (`PATH=/usr/bin:/bin` and the few
variables set by the harness). Nothing reads real provider state, a shell
profile or the network, and no real `claude` or `codex` is started. `delega`
runs in a process group of its own, so no signal the harness sends reaches the
harness. The harness kills every process it finds by marker after each case,
including the cases that are designed to leave strays, and sweeps again at the
end.

## Live suite

`--live` runs the installed `claude` and `codex` through the built `delega`
binary, with the real `HOME` and the caller's provider logins. It calls the
real models and costs money, so nothing live runs without the flag, it is never
run in CI, and `cargo test` never runs it. `--provider` without `--live` is
refused, and so is `--live` with a matrix option (`--all`, `--smoke`,
`--filter`, `--timing-gaps`, `--repeat`, `--jobs`, `--report`, `--results`,
`--sweep`, `--allow-skips`).

```sh
cargo run -p delega-acceptance -- --live                       # both providers
cargo run -p delega-acceptance -- --live --provider claude     # one provider, no doctor
cargo run -p delega-acceptance -- --live --list                # the case ids, nothing run
cargo run -p delega-acceptance -- --live --out acceptance/live.jsonl
cargo run -p delega-acceptance -- --report                     # then render it into RESULTS.md
```

Every Claude run asks for `--model haiku` and every Codex run for
`--effort low`. Prompts are one or two sentences that say exactly what to do.
Cases run one at a time.

### Verdicts

A case ends `pass`, `fail` or `not_shown`. `fail` means something asserted did
not hold. `not_shown` means nothing asserted failed, but a check the case
exists for could not be made on this run, so the case shows less than it is
for; the reason is printed with it and recorded. It is not a pass. The run
exits non-zero when a case fails; `not_shown` cases are counted and listed in
the summary and do not change the exit status.

### Cases

With both providers selected, every case runs for each. With `--provider`,
`doctor` is skipped: `delega doctor` has no option to check one provider, and
it starts both providers' version probes. Every `delega run` must print one
valid result that names the case's provider and whose exit status follows its
outcome, with `cleanup.confirmed` true. Beyond that:

| Case | Mode | What it asserts |
| --- | --- | --- |
| `doctor` | | `delega doctor --output json` reports the provider `ready`; its version number is recorded. |
| `plain-answer` | `read` | `completed` with a non-empty answer. |
| `read-cwd` | `read` | `completed`, and the answer has the word the harness seeded in `word.txt` in the working directory. |
| `write-refused` | `read` | `completed` or `refused`, and the seeded `note.txt` is unchanged. That alone does not show a refused write, because the worker may not have tried. So the case also needs one of: a denial of a write in `permissions.denied` (Claude Code: a `Write`, `Edit`, `NotebookEdit` or `Bash` request; Codex: any declined command), or a launch plan that offers no way to write, read from `delega run --dry-run` with the same arguments (Claude Code: `--tools` names none of those four tools; Codex: `sandbox_mode="read-only"`). With neither, the case is `not_shown`. In `read` the plan offers Claude Code no write tool, so the case shows that the write was not offered, not that one was attempted and denied. |
| `read-outside` | `read` | Claude: `completed` or `refused`, the answer does not have the word seeded in a file outside the working directory, and an entry in `permissions.denied` names that file's path. Codex: `completed` and a `wider than requested` provider note; whether the answer has the word is recorded, not asserted, because Codex does not confine reads. |
| `build` | `build` | `completed`, and the worker ran a seeded `probe.sh` to its end. The script writes a file in the working directory and one in `$TMPDIR`, tries to write in the outside directory, and runs `/usr/bin/curl` against `https://example.com`. Asserted: the working-directory file exists; the temporary file exists in the `TMPDIR` delega was given, Claude Code's `/tmp/claude-<uid>` or elsewhere under `/tmp`; the outside file does not exist; curl did not succeed. Not shown: the network check when `/usr/bin/curl` is missing or the harness itself cannot reach the address just before the run; the temporary-file check when the file was written anywhere else; the outside check for Codex when the outside directory is under `/tmp`. |
| `resume` | `read` | Two runs that keep their session. The first is told a word; the second resumes the first run's `session_id` and must complete, report the same session id, and give the word back. |
| `interrupt` | `build` | The worker is told to run `sleep 120`. The harness sends SIGINT to `delega run` 2 s after a `--progress` line names the shell tool (Claude Code's `Bash`, a Codex `command_execution`). Asserted: `interrupted`, exit 130. If no such line comes within 30 s, the harness sends SIGINT then and the case is `not_shown`; if `delega` ends before any signal, the case fails. |
| `timeout` | `build` | The same prompt with `--timeout 10s`. Asserted: `timed_out`, exit 124, and `timing.timeout_secs` 10. |

The interrupt goes through the documented interrupt path of the command line
(SIGINT to `delega run`, see "Interrupt" in
[docs/api.md](https://github.com/joshrotenberg/delega/blob/main/docs/api.md#command-line)),
not through MCP or the library.

Every run but the two of `resume` passes `--ephemeral`, so the providers keep
no session for it. The `resume` runs leave their two sessions in the
provider's own history under the real `HOME`; the harness does not delete
provider state.

### Directories

Each case gets a directory `dlive-XXXX` with three subdirectories: `work`, the
worker's working directory; `tmp`, given to `delega` as `TMPDIR`; and
`outside`, given to no run. It is removed when the case ends.

The build probe's temporary file has a random name made for each case
(`dlive-probe-` and 16 hex digits from `/dev/urandom`). The script records the
file's path in `work` before it makes the file, and makes it with `set -C`, so
a file already at that path is never written. Afterwards the harness removes
only a regular file (not a link) with that name at the recorded path; it
removes nothing else outside the case directory. If the script is stopped
before it makes the file, nothing is left; if the harness itself is killed, the
file can remain.

After a run the harness reads what the worker could have changed (`note.txt`,
the recorded path) only when it is a regular file, opened without blocking and
without following a link, so a FIFO left in its place cannot stall the case.

The directories are made on macOS under the per-user temporary directory that
`getconf DARWIN_USER_TEMP_DIR` names (under `/var/folders`), and elsewhere under
`XDG_RUNTIME_DIR` when it is set, else under the system temporary directory.
The aim is an `outside` directory that no `build` sandbox lets commands write:
it is not the working directory, not under `/tmp/claude-<uid>` (where Claude
Code points its commands' `TMPDIR`), not under `/tmp` (which Codex's `build`
includes), and not the `TMPDIR` delega was given (which Codex also includes).
On a system with no `XDG_RUNTIME_DIR` the directories fall back under `/tmp`;
then the outside write is still asserted for Claude Code, whose sandbox does not
let commands write the rest of `/tmp`, and is `not_shown` for Codex.

### Bounds

Every run has `--timeout 120s` (the timeout case 10 s). If `delega` has not
exited 45 s past its time limit (`doctor` and the dry run, 60 s after they
start), the harness sends SIGTERM, and SIGKILL 15 s later, and the case fails
with a problem saying so. After `delega` exits the harness waits at most 5 s
for its standard output and standard error to close; a process it started that
still holds one fails the case, with what was already written kept. Each case
has a bound of 450 s, enough for the two runs of `resume` with every
escalation: a run is given at most what is left of it, and none starts after
it. So each case ends, and the suite with it.

A SIGKILL of `delega` is a last resort: it cannot stop its worker, so the
harness prints on standard error that the provider may still be running
(delega issue 163). The harness does not look for or kill provider processes.

### Ctrl-C

`delega` runs in a process group of its own, so a Ctrl-C at the terminal
reaches only the harness. The harness catches SIGINT and SIGTERM and passes the
current `delega` its documented interrupt, SIGINT. If `delega` has not exited
20 s later, it gets SIGTERM, and SIGKILL 15 s after that, announced as above.
The case then fails, its directory is removed, no results are written, and the
harness exits 130. Further Ctrl-Cs change nothing; the stop is already bounded.

### Output

One line per case (provider, case, verdict, outcome, seconds) with the
problems and the not-shown checks of a case that is not a pass, then counts of
each verdict, then per provider the version number, the number of runs, the
cost Claude Code reports in `usage.cost_usd` (or that no cost was reported, as
for Codex) and the token counts. Nothing printed or recorded holds a model's
answer, a denial's text, the content of a file other than the words the
harness seeded, a provider's version line (only the provider name and the
first version number in it, such as `claude 2.1.330`), or an environment value.
Problems name what was asserted; an error is named by its `error.kind` only.

`--out FILE` writes one file: first a line `{"meta": {...}}` (date, machine,
delega version, harness commit, provider version numbers, the number of cases,
model, effort and seconds), then one JSON line per case (verdict, outcome,
exit status, `cleanup.confirmed`, the number of denials, whether a
`wider than requested` note was present, `error.kind`, provider version number,
seconds, reported cost and tokens, and the checks, notes, not-shown checks and
problems). It is written to a temporary file beside `FILE` and renamed into
place. `--report` renders `acceptance/live.jsonl`, when present, into a "Live
suite" section at the end of `RESULTS.md`, with the not-shown checks and notes
of each case. It refuses a file whose first line is not the metadata, or whose
metadata does not match its rows (the number of cases, a provider's version).
Without the file the section says no live run is recorded.

### Cost

A full run makes 9 model runs per provider (none for `doctor`, two for
`resume`), of which `interrupt` and `timeout` are stopped early. One run of an
earlier revision of this suite on macOS on 2026-11-22, with Claude Code 2.1.330
and codex-cli 0.171.0, passed 18 of 18 cases in 91 s; Claude Code reported
$0.0049 for its runs in all, and Codex reported no cost. That run is not
recorded in `live.jsonl`. Before it, the estimate was at most about $0.25 for
Claude Code, from two start runs in #115 (default model) that reported $0.011
and $0.028 each. For Codex, a start run in #115 reported about 16,000 input
tokens, 13,000 of them cached, and about 600 output tokens; with a ChatGPT
login that counts against the plan's usage limits and has no per-run charge,
and with an API key it is billed at the model's token prices.

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

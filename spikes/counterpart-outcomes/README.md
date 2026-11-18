# counterpart-outcomes spike

Measures counterpart's worker runner (`counterpart-core`, counterpart commit
`8fa4645`, feature `workers`) against delega acceptance criteria 1 and 2, with
the same harness shape and the same fake-provider behavior as the sibling
[due-outcomes](../due-outcomes/README.md) spike. Results are in
[RESULTS.md](RESULTS.md); the side-by-side with due is in
[../COMPARISON.md](../COMPARISON.md).

The crate depends on counterpart by path
(`../../../counterpart/crates/counterpart-core`) and expects a counterpart
checkout beside this repository's parent directory. It is not part of any
workspace; its `Cargo.lock` is counterpart's workspace lockfile.

## Run

```sh
cd spikes/counterpart-outcomes
cargo build --release
./target/release/counterpart-outcomes-spike --jobs 12 --out results.jsonl   # several minutes
./target/release/counterpart-outcomes-spike --table results.jsonl           # markdown table
```

`--filter SUBSTR`, `--list` and `--jobs N` work as in the due crate. The run
exits non-zero if any process carrying a case marker is alive at the end. It
needs `/bin/bash`, `perl` and `ps`, starts no real provider, uses no network
and gives the supervisor an explicit variable vector (`HOME` and `TMPDIR` in a
per-case temporary directory, `PATH=/usr/bin:/bin`), never the real
environment.

## How it drives counterpart

For each row, one child process (this binary re-executed with `--case`):

1. builds `ServiceConfig::from_vars_with_hook_program` from that vector, points
   both `claude` and `codex` at the case's `provider` launcher, and leaves
   every lifecycle bound at its default (`kill_grace` 1500 ms, `result_grace`
   1000 ms, `stop_deadline` 10 s, `drain_bound` 2 s, descendant tracking on);
2. starts `SupervisorHandle`, parses a spawn request with
   `worker_input::parse_spawn` (an explicit `timeout_ms` only for the rows
   that name a timeout; every other row takes counterpart's default), and
   spawns the worker;
3. polls `read` every 20 ms until the agent is `completed`, `failed` or
   `interrupted` with a null pid, or the row's hard limit (12 s for rows
   expected to hang with no deadline, 30 s otherwise) passes and the row is
   recorded as `HANG`;
4. prints a JSON line with the settled agent verbatim, then, when the parent
   sends `go`, calls `stop(close=true)` and `shutdown()` and prints a second
   line.

The parent (`src/parent.rs`, unchanged in shape from the due crate) measures
survivors 1 s and 5 s after each line, by the case-directory marker in `ps`
output and by the pid ledger the fakes write, then kills anything left.

## Cases and variants

Cases and variants are those of the due crate, with `j` meaning "the `wait`
future is dropped at 1 s" and `j2` meaning "the `SupervisorHandle` is dropped
at 1 s". The permission cases `gd` and `gr` are not run: counterpart's workers
are non-interactive turns, so there is no live request to leave unanswered.
Two variants exist only here, because counterpart tracks descendants:

- `orphan`: a child starts a `setsid` grandchild and exits at once, so the
  grandchild is reparented to init before any process listing can link it to
  the provider.
- `orphan-late`: as `orphan`, but the child lives 1.3 s first, long enough for
  two 500 ms captures.

## Fakes

`fakes/` holds the one-shot fakes from the due crate, answering the probes
counterpart runs first (`claude --version`, `codex mcp list --json`). Claude
runs in `-p --output-format stream-json` mode and Codex in `exec --json` mode,
as counterpart launches them, and the descendant behavior of every variant is
byte for byte the due fakes' (`lib.sh`, `child.sh`), plus the two orphan
variants. The same limits apply: bash 3.2 scripts, descendants in the
provider's process group, no real CLI.

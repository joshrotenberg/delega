# due-outcomes spike

Measures whether `due-core` (due commit `8d56df2`) meets the first two delega
acceptance criteria: one stated outcome per run, and no process left behind.
The measured results and the recommendation are in [RESULTS.md](RESULTS.md).

The crate depends on due by path (`../../../due/crates/due-core`), so it
expects a due checkout beside this repository's parent directory. It is not part
of any workspace and has its own `Cargo.lock`, copied from due's lockfile so
the wrapper crate versions match.

## Run

```sh
cd spikes/due-outcomes
cargo build --release
./target/release/due-outcomes-spike --jobs 12 --out results.jsonl   # several minutes
./target/release/due-outcomes-spike --table results.jsonl           # markdown table
```

Options: `--filter SUBSTR` runs only rows whose id contains the text,
`--list` prints the row ids, `--jobs N` sets concurrency. The run exits
non-zero if any process carrying a case marker is still alive at the end.

It needs `/bin/bash` (3.2 is enough), `perl` and `ps`. It starts no real
provider, uses no network and sets `HOME` to a per-case temporary directory.

## How it works

- `src/matrix.rs` lists the rows: path (`oneshot`, `resident`), provider
  (`claude`, `codex`), outcome case (`a` to `j`) and cleanup variant.
- Each row runs in its own child process (this binary re-executed with
  `--case`) and its own temporary directory named `delega-spike-<row>-<random>`.
  A blocked or crashed case cannot affect another, and concurrent rows share
  nothing.
- `fakes/` holds shell fake providers that imitate each wire format
  (one-shot Claude `-p` JSON, one-shot Codex `exec --json`, resident Claude
  stream-json duplex, resident Codex app-server JSON-RPC). The resident
  envelopes follow due's `tests/fixtures/session-agent.sh`. Behavior is chosen
  by `SPIKE_CASE` and `SPIKE_VARIANT`.
- Every process a fake starts runs a script under the case directory, so the
  directory name appears in its command line. The harness finds survivors by
  that marker in `ps` output, and by the pid ledger (`pids`) the fakes write.
  A zombie is reported as `(zombie)`.
- `src/child.rs` calls due and prints one JSON line when the call returns (and
  one more after `Engine::shutdown` on the resident path). `src/parent.rs`
  records the time of each line, measures survivors 1 s and 5 s later, kills
  whatever is still alive, and writes one record per row.
- Every call has a hard limit (12 s for calls with no deadline that are
  expected to hang, 30 s otherwise). A call that has not returned by then is
  recorded as `HANG`. The harness has a second backstop in case a case process
  prints nothing at all (`RUNNER-STUCK`).

## Cases and variants

Cases: `a` normal, `b` provider exits 3, `c` assignment timeout 2 s with
output continuing, `d` caller interrupts at 1 s, `e` provider SIGKILLed,
`f` silent hang (`fd`: with a 3 s timeout), `g` permission asked and nobody
answers (`gd` with a 3 s timeout, `gr` reject mode), `h` stdout closed while
alive (`hd` with timeout), `i` output ends without a final result (`id` with
timeout), `j` calling task cancelled or `wait` future dropped, `j2` the
`Engine` dropped.

Variants: `plain` (no descendants), `child` (one child in the provider's
process group, stdio detached), `setsid` (a child that starts a grandchild in
its own session), `ignore` (a child that ignores SIGTERM and SIGINT),
`stubborn` (the provider itself ignores SIGINT, SIGTERM and interrupt
requests) and `pipe` (a same-group child that keeps the provider's stdout
open).

## Limits of the fakes

The fakes do not behave like the real CLIs in every respect. A real Codex
stops commands it started when its app-server input closes; the fake exits on
input EOF without touching its children. Real Claude and Codex place tool
processes in their own process groups; the fakes' descendants share the
provider's group unless the variant says `setsid`. Fake provider scripts are
bash; their handling of signals and read timeouts follows bash 3.2.

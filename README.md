# delega

Run Claude Code and Codex agents from a program, for any purpose, reliably.

`delega` starts a provider CLI (`claude` or `codex`) as a worker, gives it an
assignment, and returns one result with a stated outcome. It is reached three
ways, over one library:

- `delega run`: one assignment in the foreground, for a script or a shell.
- `delega mcp`: an MCP server that supervises several workers at once, for a
  Claude Code or Codex session that hands work to other agents.
- `delega-core`: the Rust library both are built on.

A request says which provider, what to do, in which directory, what the worker
may do and how long it may take. The result says how the run ended, the answer,
what was denied, whether every process was cleaned up, and where the provider
did something other than what was asked.

## Install

Requirements: macOS or Linux, Rust 1.90 or later, and Claude Code, Codex or both
installed and logged in. Windows is not supported. There is no release yet
([#140](https://github.com/joshrotenberg/delega/issues/140)), so build from
source:

```sh
git clone https://github.com/joshrotenberg/delega
cd delega
cargo install --locked --path crates/delega
```

Check what a run would meet, without starting a worker:

```sh
delega doctor
```

It reports each provider's executable and version, the environment and deny
rules a worker would get, the configuration file, the limits in force and
anything that would stop a run. It exits 0 when a run can go ahead, and 1 when
no provider is ready, the configuration file cannot be used or process
inspection does not work. See [Doctor](docs/api.md#doctor).

To use the MCP tools and skills from Claude Code or Codex, install the plugin:

```sh
delega plugin install --dry-run   # print the files and host commands, change nothing
delega plugin install             # every host found
delega plugin install --host claude
```

It copies the running `delega` into `~/.local/share/delega` and registers it
through the hosts' own commands (`claude plugin ...`, `codex plugin ...`). Run it
again after rebuilding to refresh the plugin. `delega plugin uninstall` removes
it. See [Install and uninstall](docs/api.md#install-and-uninstall).

A running host session keeps the server it started with. After an install, start
a new session, or in Claude Code reconnect `plugin:delega:delega` from `/mcp`
(`/reload-plugins` does not restart the server). Until then, `list` and
`delega://identity` report `stale_server` when the executable was replaced.

## Quickstart

### Command line

```sh
delega run --provider codex -p "Summarize this repository"
```

With the default `--output text`, standard output holds the answer and nothing
else, and only when the run completed. A run that did not complete prints one
line on standard error, `delega run: <outcome>: <error.kind>: <error.message>`.
`--output json` prints the whole result. The exit status follows the outcome:

| Outcome | Exit status |
| --- | --- |
| `completed` | 0 |
| `failed` | 1 |
| `refused` (including usage errors) | 2 |
| `timed_out` | 124 |
| `interrupted` | 130 |

More forms:

```sh
delega run --provider claude --stdin --output json < task.txt > result.json
delega run --provider codex --write --timeout 20m -p "Fix the failing test"
delega run --provider claude --allow-command "cargo test" -p "Run the tests and report"
delega run --provider codex --resume SESSION_ID -p "Now add tests"
delega run --provider claude -p "Plan the change" --dry-run
```

`delega run --help` lists every option. [Command line](docs/api.md#command-line)
describes profiles, `--progress`, `--env` and the configuration file.

### MCP, from a host session

With the plugin installed, the host has seven tools: `start`, `follow_up`,
`wait`, `read`, `list`, `interrupt` and `close`. The `delegate` skill that comes
with the plugin tells the host agent how to use them. A typical sequence:

1. `start` returns at once with a run id and a worker id. `cwd` must be an
   absolute path.

   ```json
   {"provider": "codex", "prompt": "Review the diff on this branch", "cwd": "/abs/path/to/repo"}
   ```

2. `wait` returns when a named run finishes or after `timeout_secs` (30 by
   default, at most 300). A timeout is not an error. Each response has a
   `cursor`, a number; pass it back as `after` on the next call, and the wait
   returns only for runs that finished after it.

   ```json
   {"runs": ["RUN_ID"], "timeout_secs": 60, "after": 7}
   ```

3. `read` returns the result once the run has finished, or its progress before.

   ```json
   {"run": "RUN_ID"}
   ```

4. `close` stops the worker and frees its slot. `follow_up` before it continues
   the same provider session.

   ```json
   {"worker": "WORKER_ID"}
   ```

Every tool's input and output schema is under [`schemas/mcp`](schemas/mcp).
See [MCP](docs/api.md#mcp) and [`skills/delegate/SKILL.md`](skills/delegate/SKILL.md).

### Library

`delega-core` is not published to crates.io. Depend on it from git:

```toml
[dependencies]
delega-core = { git = "https://github.com/joshrotenberg/delega" }
```

One run, from the body of an async function on a Tokio runtime. The same lines
are the `no_run` doc test in
[`crates/delega-core/src/provider/mod.rs`](crates/delega-core/src/provider/mod.rs),
which `cargo test` compiles:

```rust
use delega_core::contract::request::{Provider, Request};
use delega_core::provider::{ProviderOptions, run};

let request = Request::new(Provider::Codex, "Summarize this repository");
let result = run(request, ProviderOptions::default()).await;
println!("{:?}", result.outcome);
```

Several workers at once go through `delega_core::supervisor::Supervisor`, whose
module documentation has the same kind of example. See
[docs/supervisor.md](docs/supervisor.md).

## Guarantees

These six points are the acceptance criteria for the core. Nothing else is
ported until the core meets them.

1. **One stated outcome.** Every run ends in exactly one of: completed, failed,
   timed out, interrupted, refused. Never a hang and never silence.
2. **No process left behind.** After any outcome, the provider process and its
   descendants are gone, or the result says cleanup could not be confirmed and
   what remains.
3. **Explicit permissions.** What a worker may do is stated up front or asked
   live, with a defined answer when nobody is there to answer.
4. **One request, either provider.** The same request launches on Claude Code or
   Codex. Where the providers differ, the result names the difference.
5. **A versioned contract.** Requests and results are documented, versioned and
   the same from the CLI, the library and MCP.
6. **Proven.** Tests against fake providers cover every outcome. A small live
   suite per provider is run locally by a maintainer before a release.

Where each is enforced and how it is tested:

| # | Enforced in | Tested by |
| --- | --- | --- |
| 1 | The `Outcome` type and `RunResult::validate` (`crates/delega-core/src/contract`); time limits in the process layer (`process`); the provider layer maps every ending to one result (`provider/settle.rs`); the supervisor replaces a result that fails validation with `failed`. | `provider/tests/outcomes.rs`, `crates/delega/tests/run_outcomes.rs`, `crates/delega-core/tests/supervisor`, and the acceptance matrix in CI on `ubuntu-latest` and `macos-latest`. |
| 2 | The process layer (`crates/delega-core/src/process`): process group and session sweeps, descendant tracking, repeated signalling, and a `cleanup` verdict that names what remains. | `process/tests` and the acceptance matrix, which measures what is still running 1 s after `delega` exits. Known gaps are listed under [Status and limits](#status-and-limits). |
| 3 | `permissions` in the request (`contract/permissions.rs`), mapped per provider (`provider/permissions.rs`, `claude.rs`, `codex.rs`, the command guard in `hook.rs`, deny rules in `deny.rs`). A request not granted is answered no and listed in `permissions.denied`. Live asking is not built. | `provider/tests/enforce.rs`, `mapping.rs`, `strict.rs`, `hook.rs`, `deny.rs`, with fakes that honour the permission arguments; parts are verified against the real CLIs ([providers.md](docs/providers.md#verified-against-the-real-cli)). |
| 4 | One `Request` type with a `provider` field; each adapter reports what it could not do as asked in `provider_notes`, or refuses with `permissions.strict`. | The provider tests, against a fake of each CLI (`fake-claude.sh`, `fake-codex.sh`); the acceptance matrix runs the same 94 rows on each provider. |
| 5 | `schema_version` 1 on the request and result (`crates/delega-core/src/contract`); schemas generated from the types into [`schemas`](schemas); the CLI, the MCP server and the library use the same types. | `committed_schemas` in `contract/schema.rs` and `crates/delega-mcp/tests/committed_schemas.rs` fail when a committed schema is stale. |
| 6 | | Fake providers: `crates/delega-core/src/provider/tests/fakes`, run by `cargo test --workspace` and the acceptance matrix on every pull request. The live suite is not built yet ([#139](https://github.com/joshrotenberg/delega/issues/139)). |

The latest committed acceptance run ([acceptance/RESULTS.md](acceptance/RESULTS.md),
188 rows on Darwin arm64, fake providers only): every row met its expectation or
is a listed expected failure. In each of the 172 rows where the harness neither
closed standard output nor killed `delega`, `delega` printed exactly one valid
result and exited. The expected failures are the documented gaps of criterion 2
below.

## Status and limits

- **Version** 0.1.0-dev. No release; build from source.
- **Platforms.** macOS and Linux. CI runs fmt, clippy, the tests and the full
  acceptance matrix on `macos-latest` and `ubuntu-latest`, and builds on Rust
  1.90. Windows is not supported.
- **Use.** Since 2026-11-19 delega's own development has run on real Claude Code
  and Codex workers through the installed plugin, as implementers and as
  independent reviewers: more than a hundred worker runs behind ten merged pull
  requests between 2026-11-20 and 2026-11-22.
- **Verified against the real CLIs** (Claude Code 2.1.328 and 2.1.329, codex-cli
  0.170.0): permission scoping, the command guard, deny rules, the Codex
  sandbox, session resume, version parsing, usage reporting and several failure
  shapes. Other provider behaviour is assumed and tested against fakes only. Both
  lists are in [providers.md](docs/providers.md#verified-against-the-real-cli).
- **Permission modes.** `read` (default), `write` and `full`, plus exact
  commands (`--allow-command`) and extra readable directories. Work that builds
  and runs tests needs `full` today, because `read` and `write` allow only
  commands listed exactly ([#98](https://github.com/joshrotenberg/delega/issues/98)).
  Codex confines neither reads nor commands, so a `read` or `write` Codex run is
  wider than requested and says so in `provider_notes`; with `--strict` it is
  refused. A permission request not granted is answered no; asking the caller
  live is not built.
- **Cleanup gaps.** Cleanup reports confirmed while a process still runs when a
  descendant called `setsid` and was orphaned before any process listing saw it,
  or when a fast fork relay is never listed long enough. A `SIGKILL` of `delega`
  itself leaves the worker running. These are the 30 documented-gap rows in
  [acceptance/RESULTS.md](acceptance/RESULTS.md), described in
  `crates/delega-core/src/process/mod.rs`.
- **Environment.** A worker gets a fixed environment allowlist plus what `--env`
  or the configuration file adds. The installed plugin starts `delega mcp` with
  no `--env`.
- **Hosts.** Claude.ai and Cowork refuse plugins with a top-level `bin/`, so the
  plugin installs only into Claude Code and Codex.

## Scope

In scope now:

- The runner: start, follow up, interrupt, wait, read the result.
- Surfaces: CLI and MCP, over one library.

Later, as layers over the runner, once the core meets the six points:

- Enforced independent review of a worker's change (counterpart's pair).
- Work loops and other orchestration.

Out of scope:

- Session history. That is [trovador](https://github.com/joshrotenberg/trovador).
- A JavaScript harness and a JSON-RPC surface. due has both; they are left out
  until the core is solid and then reconsidered.

## Documentation

| Document | Covers |
| --- | --- |
| [docs/api.md](docs/api.md) | The request and result, outcomes, the CLI and its exit statuses, `doctor`, plugin install, the configuration file (`~/.config/delega/config.toml`), the MCP tools, skills, the library, and what is deferred. |
| [docs/supervisor.md](docs/supervisor.md) | The supervisor: operations, worker and run states, limits and defaults, cursors, retention. |
| [docs/providers.md](docs/providers.md) | How each provider is started, the permission mapping, error kinds, the environment policy, managed settings, and what is verified against the real CLIs. |
| [acceptance/README.md](acceptance/README.md) | The acceptance harness for criteria 1 and 2 and its matrix. |
| [skills](skills) | The `run` and `delegate` skills built into the executable (`delega skill list`). |

## Development

| Path | Holds |
| --- | --- |
| `crates/delega-core` | The library: the contract (`contract`), the process layer (`process`: one child, one terminal report, cleanup), the provider layer (`provider`: a request to a Claude Code or Codex child and back to one result), the supervisor (`supervisor`), the configuration file (`config`), profiles, `doctor` and the embedded skills. |
| `crates/delega-mcp` | The MCP server over the supervisor: the seven tools, the skills and the identity as resources, the `stale_server` report, and serving on standard input and output. Its tests run the router in process against the fake providers. |
| `crates/delega` | The `delega` binary: one module per subcommand under `src/commands/`. Its tests run the built binary against the fake providers kept in the core crate. |
| `acceptance` | `delega-acceptance`, which drives the built `delega` against fake providers through a matrix of behaviours and caller actions and measures what was reported and what was left running. |
| `schemas` | JSON Schemas generated from the types: request, result, doctor report, configuration file, plugin install and uninstall reports, and under `schemas/mcp` each MCP tool. |
| `skills` | The skills embedded in the binary and installed with the plugin. |
| `docs` | The design documents listed above. |

Run Cargo from the repository root. These are the checks CI runs:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`cargo test --workspace` runs a handful of acceptance rows. The whole matrix:

```sh
cargo run --release -p delega-acceptance -- --all
```

It writes `acceptance/results.jsonl`; the committed run took 82 s with 8 cases
at a time on Darwin arm64 ([acceptance/RESULTS.md](acceptance/RESULTS.md)).

After changing a contract type, regenerate the schemas with
`DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas`.
Read [AGENTS.md](AGENTS.md) before contributing.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

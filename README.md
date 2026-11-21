# delega

Run Claude Code and Codex agents from a program, for any purpose, reliably.

`delega` starts a provider CLI as a worker, gives it an assignment, and returns a
definite result. It is reached from a command line, from Rust, and over MCP.

This repository is new. The request and result contract, the process layer, the
provider layer, the supervisor (several workers at once, as a library) and the
MCP server over it exist so far. `delega run`, `delega mcp`,
`delega plugin install`, `delega plugin uninstall`, `delega schema request|result`,
`delega skill list|show NAME`, `delega doctor` and `delega version` work.
`delega plugin install` and `delega plugin uninstall` have been tested against
fake host CLIs only.
`delega mcp` serves seven tools (`start`, `follow_up`, `wait`, `read`, `list`,
`interrupt`, `close`) over standard input and output, and has been tested
against fake providers only. `delega run` runs one assignment on a
Claude Code or Codex worker, with profiles, `--stdin`, `--dry-run`,
`--progress`, `--allow-command` and the exit statuses in
[docs/api.md](docs/api.md); it has been tested against fake providers only, not
against the real CLIs. It merges two earlier
projects, [due](https://github.com/joshrotenberg/due) and
[counterpart](https://github.com/joshrotenberg/counterpart), and the first work
is deciding what to carry over and proving the core. The design record that led
here is counterpart issue 223.

## What "reliably" means

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
   suite per provider runs in CI.

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

## Layout

| Path | Holds |
| --- | --- |
| `crates/delega-core` | The library: the request and result contract, the process layer (`process`: one child, one terminal report, cleanup), the provider layer (`provider`: a request to a Claude Code or Codex child and back to one result) and the supervisor (`supervisor`: several workers at once, with follow-ups, a queue, limits, wait, read, interrupt and close). |
| `schemas` | JSON Schemas for the request and result, generated from the core types, the doctor report, the optional user-level configuration file (`config.schema.json`, described in `docs/api.md`), and under `schemas/mcp` the input and output schemas of each MCP tool. |
| `crates/delega-mcp` | The MCP server over the supervisor: the seven tools, the skills and the identity as resources, and serving on standard input and output with an orderly stop. Its tests run the router in process against the fake providers. |
| `crates/delega` | The `delega` binary: one module per subcommand under `src/commands/`, and `run/` split into request building, execution, signals, progress and output. Its tests run the built binary against the fake providers kept in the core crate. |
| `acceptance` | `delega-acceptance`, the acceptance test for criteria 1 and 2: it drives the built `delega` against fake providers through a matrix of behaviours and caller actions and measures what was reported and what was left running. Results are in `acceptance/RESULTS.md`. |
| `docs/api.md` | The agreed design for the CLI and MCP surface, including the optional user-level configuration file (`~/.config/delega/config.toml`). |
| `docs/supervisor.md` | The supervisor API, the worker and run states with their transition tables, limits and defaults, retention, and what is not built. |
| `docs/providers.md` | How each provider is started: command shape, permission mapping, `native_args` allowlist, environment policy, and what is unverified against the real CLIs. |

Run Cargo from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The acceptance harness runs the whole matrix on demand; `cargo test --workspace`
runs only a handful of its rows:

```sh
cargo run --release -p delega-acceptance -- --all
```

It takes about 75 seconds and writes `acceptance/results.jsonl`. See
[acceptance/README.md](acceptance/README.md) for the options, the matrix and how
to regenerate `acceptance/RESULTS.md` and `acceptance/COMPARISON.md`.

## The API

[docs/api.md](docs/api.md) describes the contract, the CLI, the MCP server and
the skills. Everything in it is built except the parts marked **Later** and the
items listed under Deferred at the end.

A failed run's `error.kind` says why it failed, including account, billing,
capacity, model and context failures the provider reports (`provider_auth`,
`provider_capacity`, `provider_model_unavailable` and others); see the
[error kinds](docs/providers.md#error-kinds).

After changing a contract type, regenerate the schemas with
`DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas`.

## Where the parts come from

| Part | Starting point | Why |
| --- | --- | --- |
| Worker transport, request and result contracts, CLI `run` | due | Long-lived native sessions with live permission requests; versioned schemas; CI |
| Process cleanup, deadlines, queue | counterpart | Descendant tracking, repeated signalling, settlement diagnostics |
| Review layer | counterpart | Snapshots with digests, checkpoints, findings, final review |

Whether due's engine can meet points 1 and 2 is unproven. That is the first
thing to establish.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

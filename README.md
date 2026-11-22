# delega

Run Claude Code and Codex agents from a program.

`delega` starts a provider CLI (`claude` or `codex`) as a worker, gives it an
assignment, and returns one result with a stated outcome. It is reached three
ways, over one library:

- `delega run`: one assignment in the foreground, for a script or a shell.
- `delega mcp`: an MCP server that supervises several workers at once, for a
  Claude Code or Codex session that hands work to other agents.
- `delega-core`: the Rust library both are built on.

## Install

Requirements: macOS or Linux, Rust 1.90 or later, and Claude Code, Codex or both
installed and logged in. There is no release yet
([#140](https://github.com/joshrotenberg/delega/issues/140)):

```sh
git clone https://github.com/joshrotenberg/delega
cd delega
cargo install --locked --path crates/delega
delega doctor     # what a run would meet, without starting a worker
```

[Install](https://github.com/joshrotenberg/delega/blob/main/docs/getting-started/install.md)
has the details and [Plugin setup](https://github.com/joshrotenberg/delega/blob/main/docs/getting-started/plugin.md)
covers the MCP tools and skills in Claude Code and Codex.

## Example

```sh
delega run --provider codex -p "Summarize this repository"
```

A completed run prints the answer and exits 0; a failed run exits 1, a refused
one 2 and one that timed out 124. `--output json` prints the whole result.
[First run](https://github.com/joshrotenberg/delega/blob/main/docs/getting-started/first-run.md)
explains the result and the other cases.

## Acceptance criteria

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
6. **Tested.** Tests against fake providers cover every outcome. A small live
   suite per provider is run locally by a maintainer before a release.

Where each stands:

| # | Status | Where |
| --- | --- | --- |
| 1 | Met against fake providers. | `contract`, `process`, `provider/settle.rs`; tested by the provider, CLI and supervisor tests and the acceptance matrix. |
| 2 | Partly met. The result reports the cleanup problems delega detects; the documented gaps ([Status and limits](https://github.com/joshrotenberg/delega/blob/main/docs/project/status.md)) can leave a process running while cleanup is reported as confirmed. | `process`; tested by `process/tests` and the acceptance matrix. |
| 3 | Partly met. Permissions are stated up front, and a request not granted is answered no. Asking the caller live is not built. | `contract/permissions.rs`, `provider`; tested by `provider/tests` against fakes that honour the permission arguments. |
| 4 | Met for the differences delega can identify, which it reports in `provider_notes`. With `permissions.strict`, a run with a permission that delega identifies as one the provider cannot enforce is refused with `permissions_not_enforceable`. | `provider/claude.rs`, `provider/codex.rs`; tested against a fake of each CLI. |
| 5 | Met. Request and result carry `schema_version` 1; the schemas in [`schemas`](https://github.com/joshrotenberg/delega/tree/main/schemas) are generated from the types. | `contract`; a test fails when a committed schema is stale. |
| 6 | Partly met. The fake-provider tests run in CI; the live suite is pending ([#139](https://github.com/joshrotenberg/delega/issues/139)). | `crates/delega-core/src/provider/tests/fakes`. |

The committed acceptance run ([acceptance/RESULTS.md](https://github.com/joshrotenberg/delega/blob/main/acceptance/RESULTS.md),
fake providers only, Darwin arm64) has 188 rows: 158 pass, 29 are expected
failures, 1 is a documented gap that did not show in that run, and none failed
unexpectedly.

## Documentation

The documentation is an mdBook in [`docs`](https://github.com/joshrotenberg/delega/tree/main/docs).
It is not published yet; the pages read on GitHub:

- [Introduction](https://github.com/joshrotenberg/delega/blob/main/docs/introduction.md):
  what delega is, its surfaces, and when to use it.
- Getting started:
  [Install](https://github.com/joshrotenberg/delega/blob/main/docs/getting-started/install.md),
  [First run](https://github.com/joshrotenberg/delega/blob/main/docs/getting-started/first-run.md),
  [Plugin setup](https://github.com/joshrotenberg/delega/blob/main/docs/getting-started/plugin.md).
- Guides:
  [implementation and independent review](https://github.com/joshrotenberg/delega/blob/main/docs/guides/implement-and-review.md),
  [cross-provider review](https://github.com/joshrotenberg/delega/blob/main/docs/guides/cross-provider-review.md),
  [scripts and CI](https://github.com/joshrotenberg/delega/blob/main/docs/guides/scripts-and-ci.md),
  [read-only analysis](https://github.com/joshrotenberg/delega/blob/main/docs/guides/read-only-analysis.md),
  [several workers over MCP](https://github.com/joshrotenberg/delega/blob/main/docs/guides/mcp-workers.md),
  [checking a machine with doctor](https://github.com/joshrotenberg/delega/blob/main/docs/guides/doctor.md).
- Reference:
  [The delega API](https://github.com/joshrotenberg/delega/blob/main/docs/api.md)
  (contract, CLI, doctor, plugin install, configuration file, MCP tools, skills,
  library),
  [The supervisor](https://github.com/joshrotenberg/delega/blob/main/docs/supervisor.md),
  [Providers](https://github.com/joshrotenberg/delega/blob/main/docs/providers.md).
- Project:
  [Guarantees and how they are tested](https://github.com/joshrotenberg/delega/blob/main/acceptance/README.md),
  [Status and limits](https://github.com/joshrotenberg/delega/blob/main/docs/project/status.md).

## Development

Run Cargo from the repository root. These are the checks CI runs:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`cargo test --workspace` runs a handful of acceptance rows. The whole matrix,
which writes `acceptance/results.jsonl`:

```sh
cargo run --release -p delega-acceptance -- --all
```

After changing a contract type, regenerate the schemas with
`DELEGA_UPDATE_SCHEMAS=1 cargo test -p delega-core committed_schemas`.

Build the documentation book with `mdbook build`, preview it with
`mdbook serve`, and lint it with `mdbook-lint lint docs`.

Read [AGENTS.md](https://github.com/joshrotenberg/delega/blob/main/AGENTS.md) before contributing.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/joshrotenberg/delega/blob/main/LICENSE-APACHE) or
[MIT license](https://github.com/joshrotenberg/delega/blob/main/LICENSE-MIT), at your option.

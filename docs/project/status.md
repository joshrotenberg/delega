# Status and limits

The [README](https://github.com/joshrotenberg/delega/blob/main/README.md) is
canonical for what exists and states where each of the six acceptance criteria
stands. This page lists the limits a user meets.

- **Version** 0.1.0-dev. No release; build from source
  ([#140](https://github.com/joshrotenberg/delega/issues/140)).
- **Platforms.** macOS and Linux. CI runs fmt, clippy, the tests and the full
  acceptance matrix on `macos-latest` and `ubuntu-latest`, and builds on Rust
  1.90. Windows is not supported.
- **Use.** delega's own development runs on real Claude Code and Codex workers
  through the installed plugin, as implementers and as independent reviewers:
  more than a hundred worker runs behind ten merged pull requests between
  2026-11-20 and 2026-11-22.
- **Verified against the real CLIs** (Claude Code 2.1.328 and 2.1.329, codex-cli
  0.170.0): Claude Code's read scoping, command guard and deny rules; that
  Codex's `workspace-write` sandbox blocks writes outside the working directory;
  session resume, version parsing, usage reporting and several failure shapes.
  [Providers](../providers.md#verified-against-the-real-cli) has the list of
  live checks and the
  [remaining assumptions](../providers.md#unverified-against-the-real-cli).
- **Permission modes.** `read` (default), `write`, `build` and `full`, plus
  exact commands (`--allow-command`) and extra readable directories. `build`
  runs any command inside the provider's OS sandbox, which lets commands write
  to the working directory and temporary directories and gives them no
  network, for workers that build and run tests
  ([#98](https://github.com/joshrotenberg/delega/issues/98)). A fixed command
  can be granted with `--allow-command` in `read` and `write`.
  Codex confines neither reads nor commands, so a `read`, `write` or `build`
  Codex run is wider than requested and says so in `provider_notes`; with `--strict` it is
  refused ([Wider than requested](../providers.md#wider-than-requested)).
- **Asking permission live** works for Claude Code runs over MCP or the Rust
  supervisor (`permissions.on_request: ask`). Codex runs and `delega run` refuse
  it before start ([#164](https://github.com/joshrotenberg/delega/issues/164)).
- **Cleanup.** Met on Linux; partly met on macOS. Every provider process the
  CLI and MCP start is held by a keeper, a small `delega` process that stops the
  provider's whole tree when delega is gone, even after a `SIGKILL`. On Linux
  the keeper also adopts every orphaned descendant, so cleanup cannot miss one.
  On macOS two cases can still be missed and reported as confirmed: a detached
  orphan running a program whose environment macOS hides (Apple's own programs
  under SIP, such as `/bin/sh`, `/bin/sleep` or `perl`), and a fast fork relay;
  fork tracking on macOS is
  [#167](https://github.com/joshrotenberg/delega/issues/167). On every
  platform, a run is reported unconfirmed when a process still held the output
  after the kill, a `SIGKILL` of the keeper alone makes the result unconfirmed,
  and a `SIGKILL` of the keeper together with delega leaves the worker running.
  A program that embeds the library without a keeper keeps the earlier gaps,
  and the result says so in `provider_notes`. Details are in
  `crates/delega-core/src/process/mod.rs` ("What remains possible") and the
  expected failures in
  [acceptance/RESULTS.md](https://github.com/joshrotenberg/delega/blob/main/acceptance/RESULTS.md).
- **Environment.** A worker gets a fixed environment allowlist plus what `--env`
  or the configuration file adds. The installed plugin starts `delega mcp` with
  no `--env`.
- **Hosts.** Claude.ai and Cowork refuse plugins with a top-level `bin/`, so the
  plugin installs only into Claude Code and Codex.
- **Managed MCP configuration.** Claude Code workers cannot run on a machine
  where a `managed-mcp.json` that Claude Code can read and parse is deployed:
  delega refuses such runs up front with `managed_mcp_exclusive`, and
  `delega doctor` reports it. See
  [Managed settings](../providers.md#managed-settings).
- **Live suite.** The fake-provider tests run in CI. A maintainer runs the live
  suite per provider locally with
  `cargo run -p delega-acceptance -- --live`: nine cases per provider against
  the installed `claude` (`--model haiku`) and `codex` (`--effort low`),
  never in CI. The recorded run of 2026-11-23 passed 18 of 18 cases; it is
  in the "Live suite" section of
  [acceptance/RESULTS.md](https://github.com/joshrotenberg/delega/blob/main/acceptance/RESULTS.md).
  [Guarantees and how they are tested](../acceptance.md), under "Live suite", lists the
  cases.

## Acceptance run

The committed acceptance run
([acceptance/RESULTS.md](https://github.com/joshrotenberg/delega/blob/main/acceptance/RESULTS.md),
fake providers only, Darwin arm64) has 188 rows: 158 pass, 29 are expected
failures, 1 is a documented gap that did not show in that run, and none failed
unexpectedly. [Guarantees and how they are tested](../acceptance.md) describes
the harness.

## Scope

The design decisions behind this scope are listed in
[Decisions](decisions.md).

In scope now:

- The runner: start, follow up, interrupt, wait, read the result.
- Surfaces: CLI and MCP, over one library.

Later, as layers over the runner, once the core meets the six criteria:

- Enforced independent review of a worker's change.
- Work loops and other orchestration.

Out of scope:

- Session history. That is
  [trovador](https://github.com/joshrotenberg/trovador).
- [due](https://github.com/joshrotenberg/due)'s JavaScript scripting harness
  and its standalone JSON-RPC interface. They are left out until the core is
  solid and then reconsidered.

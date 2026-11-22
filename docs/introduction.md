# Introduction

delega runs Claude Code and Codex agents from a program.

It starts a provider CLI (`claude` or `codex`) as a worker, gives it an
assignment, and returns one result with a stated outcome. A request says which
provider, what to do, in which directory, what the worker may do and how long it
may take. The result gives the outcome and the answer, and reports the denials,
cleanup problems and provider differences delega detects.

Every run ends in exactly one of five outcomes: `completed`, `failed`,
`timed_out`, `interrupted` or `refused` ([Outcomes](api.md#outcomes)).
`completed` says the provider finished; it does not say the work is correct.

## Three surfaces, one library

- **`delega run`** runs one assignment in the foreground and exits, for a script
  or a shell. It prints the answer, or the whole result as JSON, and its exit
  status follows the outcome. See [First run](getting-started/first-run.md).
- **`delega mcp`** is an MCP server that supervises several workers at once:
  start them, keep working, collect results. It is meant for a Claude Code or
  Codex session that hands work to other agents, and `delega plugin install`
  registers it in those hosts together with delega's skills. See
  [Plugin setup](getting-started/plugin.md).
- **`delega-core`** is the Rust library both are built on. `delega run` uses its
  provider layer for one run; `delega mcp` uses its `Supervisor` for several
  ([Use the library](getting-started/install.md#use-the-library),
  [The supervisor](supervisor.md)).

The request and the result are the same versioned JSON documents on all three
surfaces ([The request](api.md#the-request), [The result](api.md#the-result)).

## When to use it

- A script, a CI job or another agent needs one assignment done by Claude Code
  or Codex, with a bounded time, stated permissions and an exit status it can
  act on: use `delega run`.
- A Claude Code or Codex session should hand work to other agents and keep going
  while they run: install the plugin and use the MCP tools.
- A Rust program needs to start and supervise workers itself: depend on
  `delega-core`.
- The same request should run on either provider, with the differences named in
  the result's `provider_notes` ([Providers](providers.md)).

## When another tool fits better

- **Windows.** delega supports macOS and Linux only.
- **Asking permission live.** A worker's permissions are stated up front, and a
  request not granted is answered no. Asking the caller during a run is not
  built ([Deferred](api.md#deferred)).
- **Enforced independent review** of a worker's change, and work loops, are
  planned as later layers and are not built.
- **Session history.** That is [trovador](https://github.com/joshrotenberg/trovador).
- **A JavaScript scripting harness or a standalone JSON-RPC interface.**
  [due](https://github.com/joshrotenberg/due) has both; delega leaves them out.
- **Listing or stopping runs from another process with the CLI.** `delega run`
  is foreground only; supervising several workers is done over MCP.
- **Claude.ai and Cowork.** They refuse plugins with a top-level `bin/`, so the
  plugin installs only into Claude Code and Codex.
- **Claude Code workers on a machine with a managed `managed-mcp.json`.** Where
  one that Claude Code can read and parse is deployed, delega refuses Claude
  Code runs up front ([Managed settings](providers.md#managed-settings)).

[Status and limits](project/status.md) lists what is met, what is partly met and
the known gaps.

## Where to go next

1. [Install](getting-started/install.md): build from source and check the
   machine with `delega doctor`.
2. [First run](getting-started/first-run.md): one `delega run`, reading its
   result and exit status.
3. [Plugin setup](getting-started/plugin.md): the MCP tools and skills in Claude
   Code or Codex.
4. [Guarantees and how they are tested](acceptance.md): the acceptance harness
   for the first two criteria.
5. The reference: [The delega API](api.md), [The supervisor](supervisor.md) and
   [Providers](providers.md).

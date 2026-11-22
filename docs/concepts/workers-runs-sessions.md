# Workers, runs, and sessions

delega uses three nouns the same way on every surface: a request, a worker and
a run ([The model](../api.md#the-model)).

## Request

A request says what to run: a provider, an assignment, a working directory,
what the worker may do and how long it may take. The CLI builds it from flags,
MCP from tool arguments and the library from a struct. The fields are in
[The request](../api.md#the-request).

## Worker

A worker is one provider conversation. It holds the provider's session, so a
later assignment continues with the earlier context. A worker takes one
assignment at a time.

## Run

A run is one assignment given to a worker. A run ends in one outcome and
produces one result. [Outcomes and cleanup](outcomes-cleanup.md) explains how to
read it.

One request starts a worker and its first run. A follow-up starts another run in
the same worker.

## Sessions and follow-ups

delega starts one provider process per run. A follow-up continues the
conversation by passing the worker's provider session id to the next process as
`session.resume`. It copies the worker's first request with the new prompt, and
a new time limit if one is given, so the permissions, working directory, model,
instructions and answer schema stay the same.

A worker does not switch session. Its session is the one it was started to
resume, or else the first session id one of its runs reported. Some follow-ups
are refused as finished runs rather than errors, for example when the worker
was started `ephemeral` or has no session to continue. The rules and the
refusal kinds are in [Follow-ups](../supervisor.md#follow-ups).

The result's `session_id` is the provider's own id. A new worker, or
`delega run --resume`, can continue it with `session.resume`. What happens when
the provider reports a different session, or cannot find the one asked for, is
in [Outcomes](../providers.md#outcomes) under "Resumed sessions".

## States

A run is `queued`, `running` or `finished`, and `finished` is final. A worker is
`queued`, `running`, `idle` or `closed`, and `closed` is final. A worker is
`queued` when nothing of its own is running and a run waits for a running slot.
The transitions, and when each happens, are in the tables under
[States](../supervisor.md#states).

## The queue and the limits

When several workers run under the supervisor, a run beyond the running-run
limit waits `queued` instead of being refused. A start beyond the live-worker
limit, and a follow-up beyond a worker's queue limit, are refused. The limits,
their defaults and the refusal kinds are in
[Limits and defaults](../supervisor.md#limits-and-defaults). For `delega mcp`,
the `[mcp]` section of the
[configuration file](../api.md#configuration-file) can lower them and cannot
raise them.

A run's time limit does not run while the run is queued
([Timing](../supervisor.md#timing)).

## What each surface exposes

- **`delega run`** runs one assignment in the foreground and exits. It calls the
  provider layer directly and creates no worker, so its result has no `worker`
  field and there is no queue. It can continue a provider session with
  `--resume`. See [Command line](../api.md#command-line).
- **MCP** (`delega mcp`) serves the supervisor's operations as tools: `start`,
  `follow_up`, `wait`, `read`, `list`, `interrupt` and `close`. `start` requires
  an absolute `cwd`. See [MCP](../api.md#mcp).
- **The library** offers both. The provider layer's `run`, `start` and
  `dry_run` handle one run ([Entry points](../providers.md#entry-points)). The
  `Supervisor` runs several workers at once with the same operations as the MCP
  tools, plus `shutdown` ([Operations](../supervisor.md#operations),
  [Library](../api.md#library)).

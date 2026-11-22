# Workers, runs, and sessions

delega uses three nouns on every surface ([The model](../api.md#the-model)):

- A **request** says what to run: a provider, an assignment, a working
  directory, what the worker may do and how long it may take
  ([The request](../api.md#the-request)).
- A **worker** is one provider conversation. It takes one assignment at a time.
- A **run** is one assignment given to a worker. It ends in an outcome and
  produces a result ([Outcomes and cleanup](outcomes-cleanup.md)).

A request that is admitted starts a worker and its first run. A start that is
refused creates no worker; it is a finished run with outcome `refused`.

## Sessions and follow-ups

A follow-up gives a worker another assignment. It reuses the worker's first
request with a new prompt and continues the worker's provider session, so the
provider sees the earlier conversation. A worker does not switch session once
it has one. How it gets that session, and when a follow-up is refused, is in
[Follow-ups](../supervisor.md#follow-ups).

The result's `session_id` is the provider's own id. `session.resume` continues
it in a new worker or a new `delega run`.

## States, the queue and the limits

Runs and workers each have a small set of states, listed with their transitions
in [States](../supervisor.md#states). Under the supervisor, a run can wait
`queued` for a running slot, and some starts and follow-ups are refused when a
limit is reached. The limits and their defaults are in
[Limits and defaults](../supervisor.md#limits-and-defaults); `delega mcp` reads
its values from the [configuration file](../api.md#configuration-file).

## What each surface exposes

- **`delega run`** runs one assignment in the foreground and creates no worker
  ([Command line](../api.md#command-line)).
- **MCP** (`delega mcp`) serves the supervisor's operations as tools for
  several workers at once ([MCP](../api.md#mcp)).
- **The library** has the provider layer's `run` and `start` for one run, its
  `dry_run`, which describes a run without launching the provider
  ([Entry points](../providers.md#entry-points)), and the `Supervisor`
  ([Operations](../supervisor.md#operations), [Library](../api.md#library)).

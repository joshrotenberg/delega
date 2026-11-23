# Run independent workers over MCP

The MCP server runs several workers at once for one caller: start them, keep
working, collect results. A host session reaches it through the delega plugin
(`delega plugin install`; `--dry-run` first shows what it would do). The tools
are described in [MCP](../api.md#mcp) and [the supervisor](../supervisor.md).

## Start

A successful `start` call returns a run id, including for a refused run, and a
worker id only when the worker was admitted. Other failures are tool errors;
the [MCP section](../api.md#mcp) of `docs/api.md` describes them.

```json
{
  "provider": "codex",
  "name": "survey-api",
  "cwd": "/path/to/repo",
  "prompt": "List every public function in crates/api and what it returns.",
  "permissions": { "mode": "read" }
}
```

```json
{
  "provider": "claude",
  "name": "survey-cli",
  "cwd": "/path/to/repo",
  "prompt": "List every CLI flag and the function that handles it.",
  "permissions": { "mode": "read" }
}
```

- `cwd` must be absolute.
- `name` is optional and must be unique among live workers.
- Workers that only read can share a `cwd`. Give each worker that writes its
  own directory, such as its own git worktree, so their edits do not collide.
- Permissions are per worker, as for any request
  ([Permissions](../api.md#permissions)).
- Keep each `run` id: results are tracked by run id.

## Wait with the cursor

Name every worker or run you care about in one `wait`, with a short timeout,
and pass the `cursor` of each response as `after` on the next call:

```json
{ "workers": ["worker-abc-1", "worker-abc-2"], "timeout_secs": 60, "after": 17 }
```

A `wait` response is a snapshot of the named runs and workers at that moment,
with a cursor to continue from. It says that something changed, not what each
run produced: `read` the runs you started, by run id, to get their results. A
timeout is not an error and the workers keep running. The cases a response can
carry (`changed`, `unknown`, `limit_reached`, `released_by_shutdown`) are in the
[tool table](../api.md#mcp) and in
[Waiting and cursors](../supervisor.md#waiting-and-cursors).

In practice, Claude Code as the host moves a tool call that runs past 120 s to
the background and notifies the session when it returns. That is host
behaviour, not part of delega.

## Read

`read` with a `run` returns the result when it is finished, or the state and
partial progress while it runs. Long answers are paged by run id
([Reading](../supervisor.md#reading)).

## List and close

`list` returns the workers with their state and `can_follow_up`, and the limits
in force with what is in use.

`close` a worker when its result is read. It stops a running run, discards
queued ones and frees the slot at once. Results stay readable for a bounded
time after that. Workers also stop when the connection that started them closes.

## Limits

| Limit | Default | `[mcp]` key |
| --- | --- | --- |
| Live workers | 16; a `start` beyond it is refused `limit_reached` | `max_live_workers` |
| Running runs | 4; more wait `queued`, not refused | `max_running_runs` |
| Runs queued behind one worker | 16; a `follow_up` beyond it is refused `queue_full` | `max_queued_per_worker` |
| `wait` timeout | 30 s default, 300 s ceiling | `wait_default_secs`, `wait_ceiling_secs` |
| Waits blocked at once | 64 | `max_waits_in_flight` |
| Finished results kept | 256, 64 MiB, 1 hour | none; fixed for the server |

The `[mcp]` keys are `max_live_workers`, `max_running_runs`,
`max_queued_per_worker`, `wait_default_secs`, `wait_ceiling_secs` and
`max_waits_in_flight`. Each is bounded as described in the
[configuration file](../api.md#configuration-file) section. Retention has no
key and is fixed for the server. `list` and `delega doctor` report the values
in force. See [Limits and defaults](../supervisor.md#limits-and-defaults).

## What to check

- `outcome` of each run, and `error.kind` when it is not `completed`.
- A refused `start` creates no worker; its run id still reads as `refused` with
  the reason.
- `cleanup.confirmed` on every result.
- `permissions.denied` and `provider_notes`.

## Answer a permission request

Set `permissions.on_request` to `ask` for a Claude Code run. When `wait`
returns a pending interaction, inspect its tool and input summary. Call
`respond` with its `id` as `interaction` and choose `allow` or `deny`.

Whoever started the worker answers. Through the plugin that is the supervising
agent, not the person: the plugin's delegate skill tells the agent to show each
request to the person it works for and to allow only with their answer. Give
the person time with `permissions.ask_timeout` (120 seconds by default, up to
600).
Pass the wait cursor back to observe resolution or completion. If the worker
ends its turn with a question in the result text, use `follow_up` to answer.

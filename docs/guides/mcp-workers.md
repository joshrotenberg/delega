# Run independent workers over MCP

The MCP server runs several workers at once for one caller: start them, keep
working, collect results. A host session reaches it through the delega plugin
(`delega plugin install`; `--dry-run` first shows what it would do). The tools
are described in [MCP](../api.md#mcp) and [the supervisor](../supervisor.md).

## Start

Each `start` returns at once with a `worker` id, a `run` id and a state:

```json
{
  "provider": "codex",
  "name": "survey-api",
  "cwd": "/path/to/repo",
  "prompt": "List every public function in crates/api and what it returns."
}
```

```json
{
  "provider": "claude",
  "name": "survey-cli",
  "cwd": "/path/to/repo",
  "prompt": "List every CLI flag and the function that handles it."
}
```

- `cwd` must be absolute.
- `name` is optional and must be unique among live workers.
- Workers that only read can share a `cwd`. Give each worker that writes its
  own directory, such as its own git worktree.
- Permissions are per worker, as for any request
  ([Permissions](../api.md#permissions)).

## Wait with the cursor

Name every worker or run you care about in one `wait`, with a short timeout:

```json
{ "workers": ["worker-abc-1", "worker-abc-2"], "timeout_secs": 60 }
```

It returns when any of them finishes, or when the timeout passes, with each
one's state, `timed_out` and a `cursor`. Pass that cursor as `after` on the
next call:

```json
{ "workers": ["worker-abc-1", "worker-abc-2"], "timeout_secs": 60, "after": 17 }
```

With `after`, each worker and run in the response says whether it `changed`
since that cursor, and no finish is missed or reported twice. A timeout is not an
error and the workers keep running. Use this rather than polling `read`. In
Claude Code, a `wait` that runs past 120 s moves to the background and the
session is notified when it returns.

Also check `unknown` (ids that name nothing), `limit_reached` (too many waits
were open, so this one returned at once) and `released_by_shutdown`. See
[Waiting and cursors](../supervisor.md#waiting-and-cursors).

## Read

`read` with a `run` returns the result when it is finished, or the state and
partial progress while it runs. A compact read returns the first 20000
characters of the answer; page the rest with `text_offset`, by run id
([Reading](../supervisor.md#reading)).

## List and close

`list` returns the workers, each with its state, current run, queue length,
session id and `can_follow_up`, and the limits in force with what is in use.

`close` a worker when its result is read. It stops a running run, discards
queued ones and frees the slot at once. Results stay readable for a bounded
time after that. Workers also stop when the connection that started them closes.

## Limits

| Limit | Default |
| --- | --- |
| Live workers | 16; a `start` beyond it is refused `limit_reached` |
| Running runs | 4; more wait `queued`, not refused |
| Runs queued behind one worker | 16; a `follow_up` beyond it is refused `queue_full` |
| `wait` timeout | 30 s default, 300 s ceiling |
| Waits blocked at once | 64 |
| Finished results kept | 256, 64 MiB, 1 hour |

The `[mcp]` section of the [configuration file](../api.md#configuration-file)
can lower each of these. `list` and `delega doctor` report the values in force.
See [Limits and defaults](../supervisor.md#limits-and-defaults).

## What to check

- `outcome` of each run, and `error.kind` when it is not `completed`.
- A refused `start` creates no worker; its run id still reads as `refused` with
  the reason.
- `cleanup.confirmed` on every result.
- `permissions.denied` and `provider_notes`.

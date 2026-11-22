# Delegate implementation and independent review

A supervising session (Claude Code or Codex with the delega plugin installed)
hands an issue to an implementer worker, has the result read by an independent
reviewer worker, and runs the checks itself before anything is merged. This is
how delega's own changes are made; the steps below are what has worked in
practice.

The supervisor uses the MCP tools (`start`, `follow_up`, `wait`, `read`,
`close`), described in [MCP](../api.md#mcp) and the
[`delegate` skill](https://github.com/joshrotenberg/delega/blob/main/skills/delegate/SKILL.md).

## 1. Make a worktree for the implementer

The supervisor creates the worktree and branch itself, so it knows exactly where
the worker writes:

```sh
git worktree add /path/to/repo-issue-123 -b fix/issue-123 main
```

One implementer per worktree. Do not point two workers that write at the same
`cwd`.

## 2. Start the implementer

```json
{
  "provider": "claude",
  "name": "impl-issue-123",
  "cwd": "/path/to/repo-issue-123",
  "prompt": "Fix issue 123: <the issue text>. Read AGENTS.md first. Run the formatter, linter and tests. Commit on the current branch. Do not push.",
  "permissions": { "mode": "full" },
  "timeout_secs": 3600
}
```

`cwd` must be absolute, or the start is refused `cwd_required`. The worker does
not see the supervising conversation, so the prompt must stand alone.

**Permissions.** An implementer that builds and runs tests needs
`permissions.mode: full` today. `write` allows only the exact commands listed in
`permissions.commands`, which cannot cover the varying commands a build runs on
Claude Code, and Codex's `write` sandbox excludes the temporary directories
([#98](https://github.com/joshrotenberg/delega/issues/98)). `full` means no
restriction from delega, so the worktree is the boundary you rely on. See
[Permissions](../api.md#permissions) and the per-provider mappings in
[providers.md](../providers.md).

## 3. Wait and read

Call `wait` with the worker and a timeout, and pass the returned `cursor` as
`after` on the next call. See
[Run independent workers over MCP](mcp-workers.md) for the loop. In Claude Code,
a `wait` that runs past 120 s moves to the background and the session is
notified when it returns.

When the run is finished, `read` it by run id and check:

- `outcome` is `completed`. That says the provider finished, not that the work
  is correct ([Outcomes](../api.md#outcomes)).
- `cleanup.confirmed` is true.
- `permissions.denied` and `provider_notes`.
- The branch itself: `git -C /path/to/repo-issue-123 log` and the diff against
  `main`.

## 4. Review on an exported copy

The reviewer works on a copy of the exact head, so it cannot change the
implementer's tree. Export the commit and write the diff as a file:

```sh
HEAD_SHA=$(git -C /path/to/repo-issue-123 rev-parse HEAD)
mkdir /tmp/review-issue-123
git -C /path/to/repo-issue-123 archive "$HEAD_SHA" | tar -x -C /tmp/review-issue-123
git -C /path/to/repo-issue-123 diff main..."$HEAD_SHA" > /tmp/review-issue-123/review.diff
```

Start the reviewer with the other provider. See
[Review across providers](cross-provider-review.md) for the call and the
permissions. Name `HEAD_SHA` in the prompt so the review is tied to that commit.

## 5. Send fixes back to the same implementer

Use `follow_up` on the implementer's worker, so it keeps its context:

```json
{
  "worker": "worker-abc-1",
  "prompt": "The review of <HEAD_SHA> found: <findings>. Fix them and commit."
}
```

A follow-up runs with the worker's first request: the same `cwd`, permissions
and model ([Follow-ups](../supervisor.md#follow-ups)). Export and review the new
head again.

## 6. Run the checks yourself

Before merging, the supervising session runs the formatter, the linter, the
tests and any acceptance run on the exact head, in the worktree, itself. A
worker saying the tests pass is not the check. Every final change also gets at
least a short independent reading.

## 7. Close workers

`close` a reviewer as soon as its review is read, and the implementer when its
work is merged. Read results before closing: they stay readable for a bounded
time only. The supervisor has a limit on live workers (16 by default), and a
closed worker frees its slot at once
([Limits and defaults](../supervisor.md#limits-and-defaults)). Remove the
worktree and the exported copy when you are done.

## Defaults from the configuration file

A user configuration file (`~/.config/delega/config.toml`) can set what every
`start` begins from. For example, delega passes `ANTHROPIC_*` variables to
Claude workers, so this makes Claude workers that name no model use Opus:

```toml
schema_version = 1

[env.set]
ANTHROPIC_MODEL = "opus"
```

`[defaults] model` would also work, but it applies to every `start` that names
no model, whichever the provider. The server reads the file once, when it
starts. See [Configuration file](../api.md#configuration-file) and
[Environment policy](../providers.md#environment-policy).

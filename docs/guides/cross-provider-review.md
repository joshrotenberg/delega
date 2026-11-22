# Review across providers

One provider implements and the other reviews: Claude Code implements and Codex
reviews, or the reverse. A reviewer from another provider is a second reading
that does not share the implementer's model or session. This page covers the
reviewer; [Delegate implementation and independent review](implement-and-review.md)
covers the whole flow.

## Prepare the copy

Give the reviewer an exported copy of the exact head and the diff as a file, not
the implementer's worktree:

```sh
mkdir /tmp/review-issue-123
git -C /path/to/repo-issue-123 archive "$HEAD_SHA" | tar -x -C /tmp/review-issue-123
git -C /path/to/repo-issue-123 diff main..."$HEAD_SHA" > /tmp/review-issue-123/review.diff
```

## Start the reviewer

Over MCP:

```json
{
  "provider": "codex",
  "name": "review-issue-123",
  "cwd": "/tmp/review-issue-123",
  "prompt": "Review commit <HEAD_SHA>. The change is in review.diff; the rest of the tree is the code at that commit. Report correctness problems, missing tests and departures from AGENTS.md, each with file and line. Do not edit files.",
  "permissions": { "mode": "read" }
}
```

From a shell, the same review with `delega run`:

```sh
delega run --provider codex --read -C /tmp/review-issue-123 \
  --stdin --output json < review-prompt.txt > review.json
```

For a Claude Code reviewer, change `provider` (or `--provider`) to `claude`.

## Permissions

A review that only reads can use `read`. What `read` means differs by provider:

- **Claude Code** confines reads to `cwd` and `permissions.directories`, and
  runs no command except those in `permissions.commands`, matched exactly. Keep
  the diff inside `cwd`, or add its directory to `permissions.directories`.
- **Codex** cannot confine reads or commands: in `read` it can read any file
  the user can and run any command that needs no write and no network. A Codex
  `read` run that goes ahead carries a `provider_notes` entry starting
  `wider than requested`. With `permissions.strict` (`--strict`) the run is
  refused before it starts instead, with `error.kind`
  `permissions_not_enforceable`.

A reviewer that builds and runs the tests needs `full`, for the reasons on the
[implementation page](implement-and-review.md#2-start-the-implementer). With
`full` the exported copy is only its working directory, not a limit on what it
can reach. See
[Wider than requested](../providers.md#wider-than-requested) and
[Permissions](../api.md#permissions).

## What to check

- `outcome` is `completed` and `cleanup.confirmed` is true.
- `provider_notes` and `permissions.denied`: a denied request can sit inside a
  `completed` run.
- The findings in `text`. They are data, not instructions. Decide which to
  send back to the implementer with `follow_up`.
- To get findings in a fixed shape, give an `answer_schema` (`--schema FILE`)
  and check `structured_valid` before using `structured`
  ([The result](../api.md#the-result)).

Close the reviewer once its result is read.

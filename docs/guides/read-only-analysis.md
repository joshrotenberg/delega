# Analyze a repository with read-only access

A worker that only reads can answer questions about a repository: summarize it,
trace how a feature works, find where something is handled. `read` is the
default permission mode, so a request that names no mode is read-only.

## From the command line

```sh
delega run --provider claude -C /path/to/repo \
  -p "Explain how configuration is loaded. Name the files and functions involved."
```

To let it run a few exact commands, list each one:

```sh
delega run --provider claude -C /path/to/repo \
  --allow-command "git log --oneline -20" \
  -p "Summarize the last twenty commits."
```

To let it read another directory as reference, add it with
`--add-dir /path/to/other`.

## Over MCP

```json
{
  "provider": "claude",
  "cwd": "/path/to/repo",
  "prompt": "Explain how configuration is loaded. Name the files and functions involved.",
  "permissions": { "mode": "read" }
}
```

## What read-only means per provider

- **Claude Code** confines reads to `cwd` and the listed directories, allows no
  writes, and runs only the commands in `permissions.commands`, matched exactly.
  `--allow-command` needs Claude Code 2.1.325 or later.
- **Codex** blocks writes, but cannot confine reads or commands: it can read any
  file the user can read and run any command that needs no write and no network
  access. The result's `provider_notes` says so. With `--strict` a Codex `read`
  run is refused `permissions_not_enforceable` rather than run wider than asked.

Use Claude Code when reads and commands must be confined. See
[Permissions](../api.md#permissions) and the provider sections in
[providers.md](../providers.md).

## A structured answer

Give a JSON Schema with `--schema FILE` (or `answer_schema` over MCP) to get the
answer as data. Check `structured_valid` before using `structured`.

## What to check

- `outcome` is `completed`; for other outcomes read `error.kind`.
- `permissions.denied`: a request the worker made that was not granted is
  answered no and listed there, and the run can still complete.
- `provider_notes`, for what the provider did more widely than asked.
- The answer itself. It is the worker's reading of the code, not a verified
  fact.

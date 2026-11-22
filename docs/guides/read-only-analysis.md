# Analyze a repository with read-only access

A worker that only reads can answer questions about a repository: summarize it,
trace how a feature works, find where something is handled. Ask for `read`
explicitly. It is the built-in default, but a user configuration file can set a
different default mode ([Configuration file](../api.md#configuration-file)).

## From the command line

```sh
delega run --provider claude --read -C /path/to/repo \
  -p "Explain how configuration is loaded. Name the files and functions involved."
```

To let it run an exact command, list it:

```sh
delega run --provider claude --read -C /path/to/repo \
  --allow-command "git log --oneline -20" \
  -p "Summarize the last twenty commits."
```

To let it read another directory as reference, add `--add-dir /path/to/other`.

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

- **Claude Code** scopes its file tools to `cwd` and the listed directories and
  grants no editing tools. A command granted with `--allow-command` is matched
  exactly, but the match does not limit what that command does to the
  filesystem once it runs. Grant only commands you would run yourself. See the
  Claude Code [permission mapping](../providers.md#permission-mapping) and
  [commands and directories](../providers.md#commands-and-directories).
- **Codex** blocks writes but cannot confine reads or commands. See
  [Wider than requested](../providers.md#wider-than-requested).

## A structured answer

Give a JSON Schema with `--schema FILE` (or `answer_schema` over MCP) to get the
answer as data. Check `structured_valid` before using `structured`.

## What to check

- `outcome` is `completed`; for other outcomes read `error.kind`.
- `permissions.mode` in the result is `read`.
- `permissions.denied`: a request the worker made that was not granted is
  answered no and listed there, and the run can still complete.
- `provider_notes`, for what the provider did more widely than asked.
- The answer itself. It is the worker's reading of the code, not a verified
  fact.

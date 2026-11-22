# Permissions

What a worker may do is part of the request, in terms that do not depend on the
provider. delega maps it to each provider's own settings, and each provider
enforces part of it. The fields are in [Permissions](../api.md#permissions).

## Modes

- **`read`**, the built-in default: read the working directory, with no edits.
- **`write`**: also edit files in the working directory.
- **`full`**: for Claude Code, the nine built-in tools without directory
  scopes, with eligible deny rules still applied; for Codex, the sandbox is
  off.

A configuration file can change the default mode
([Configuration file](../api.md#configuration-file)), so ask for the mode you
mean. In `read` and `write`, `permissions.commands` (`--allow-command`) lists
exact commands the worker may run, and `permissions.directories` (`--add-dir`)
lists extra directories it may read. Each extra directory must exist and be a
directory.

## What each provider enforces

**Claude Code**, in `read` and `write`, scopes its file tools to the working
directory and the listed directories, and allows a listed command only through
a hook that matches it exactly. See
[Permission mapping](../providers.md#permission-mapping),
[Commands and directories](../providers.md#commands-and-directories) and
[What cannot be enforced](../providers.md#what-cannot-be-enforced).

**Codex** does not limit reads to the requested directories or commands to the
requested list. Results of runs that go ahead note what is wider than
requested. See [Permission mapping](../providers.md#permission-mapping-1) and
[Wider than requested](../providers.md#wider-than-requested).

`permissions.strict` (`--strict`) asks delega to refuse a request the provider
cannot enforce as stated, instead of running it with a note. When it applies is
in [Permissions](../api.md#permissions) and Claude Code's
[Strict](../providers.md#strict).

## Denials

The adapters record the denials they recognise in `permissions.denied`. How a
denial affects the outcome is in [Outcomes](../providers.md#outcomes).

## Deny rules

A Claude Code worker loads none of the user's or the project's Claude Code
settings except their `permissions.deny` rules, which delega adds to the worker
([Deny rules](../providers.md#deny-rules)). Codex gets none of this.

## Environment

A worker does not inherit the caller's environment. It receives an allowlist of
variables, plus the names the caller adds with `--env` or the configuration
file's `[env]` section ([Environment policy](../providers.md#environment-policy),
[Configuration file](../api.md#configuration-file)). A result of a run that got
that far reports the environment by names and counts in `applied.env`
([`applied`](../api.md#applied)).

# Permissions

What a worker may do is part of the request, in terms that do not depend on the
provider. delega maps it to each provider's own settings, and each provider
enforces part of it. The fields are in [Permissions](../api.md#permissions).

## Modes

- **`read`**, the built-in default: read the working directory and any extra
  directories.
- **`write`**: also edit files in the working directory.
- **`build`**: also run any command inside the provider's OS sandbox. Commands
  write only to the working directory and a temporary directory, and have no
  network. `permissions.commands` must be empty.
- **`full`**: for Claude Code, the nine built-in tools without directory
  scopes or a sandbox, with eligible deny rules still applied; for Codex, the
  sandbox is off.

A configuration file can change the default mode
([Configuration file](../api.md#configuration-file)), so ask for the mode you
mean. In `read` and `write`, `permissions.commands` (`--allow-command`) lists
exact commands the worker may run, and `permissions.directories` (`--add-dir`)
lists extra directories it may read. Each extra directory must exist and be a
directory.

## What each provider enforces

**Claude Code**: in `read`, its `Read`, `Glob` and `Grep` tools are scoped to
the working directory and the extra directories, and it gets no edit tools. In
`write`, `Edit` and `Write` are also granted, in the working directory only. A
command granted with `--allow-command` is matched by its text; once it runs, it
can write files or reach paths outside those scopes. In `build`, commands run in
Claude Code's sandbox, which confines their writes and refuses the network but
does not confine their reads ([Build](../providers.md#build)). See
[Permission mapping](../providers.md#permission-mapping),
[Commands and directories](../providers.md#commands-and-directories) and
[What cannot be enforced](../providers.md#what-cannot-be-enforced).

**Codex** does not limit reads to the requested directories or commands to the
requested list. In `write` and `build` its sandbox limits writes and turns the
network off; `build` also allows the temporary directories. Results of runs that go ahead note what is wider than
requested. See [Permission mapping](../providers.md#permission-mapping-1) and
[Wider than requested](../providers.md#wider-than-requested).

`permissions.strict` (`--strict`) refuses a run when the provider adapter
reports, while planning, a permission it cannot enforce as stated. It covers
only what the adapter reports; managed settings, for example, can take
precedence without a note. See [Permissions](../api.md#permissions) and Claude
Code's [Strict](../providers.md#strict).

## Denials

The adapters record the denials they recognise in `permissions.denied` (a long
list is capped, with the rest counted in a note). How a denial affects the
outcome is in [Outcomes](../providers.md#outcomes).

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

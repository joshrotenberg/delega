# Permissions

What a worker may do is part of the request, in terms that do not depend on the
provider. delega maps the request to each provider's own settings, and the
result reports what was applied, what was denied and where the provider did
something other than what was asked. The fields are in
[Permissions](../api.md#permissions).

## The three modes

| Mode | Meaning in the request |
| --- | --- |
| `read` (default) | May read the working directory. No writes. No commands except those listed. |
| `write` | May also edit files in the working directory. Commands are still only those listed. |
| `full` | No restriction from delega. |

`read` is the built-in default, but a user configuration file can set another
default mode ([Configuration file](../api.md#configuration-file)). Ask for the
mode you mean.

On the command line the modes are `--read`, `--write` and `--full`.

## Exact commands

`permissions.commands` (`--allow-command`, repeatable) lists shell commands the
worker may run in `read` and `write`, matched exactly. `full` already allows
every command. A granted command is matched as a string; the match does not
limit what the command does once it runs.

## Extra readable directories

`permissions.directories` (`--add-dir`, repeatable) lists directories the worker
may read besides the working directory. Each must exist, or the run is refused
`directory_unusable`. How a relative entry resolves depends on the surface; see
[Permissions](../api.md#permissions).

## What each provider enforces

The request is the same for both providers. What each can enforce is not.

**Claude Code** gets tool lists, allow rules scoped to the working directory
and the listed directories, and a permission mode that denies anything not
granted. Listed commands are enforced by a hook that allows a `Bash` call only
when its command equals a granted one byte for byte. Some text in a command or
a path is refused before start because it could change a rule. See
[Permission mapping](../providers.md#permission-mapping),
[Commands and directories](../providers.md#commands-and-directories) and
[What cannot be enforced](../providers.md#what-cannot-be-enforced).

**Codex** gets a sandbox mode and an approval policy of `never`. `read` blocks
writes and `write` limits writes to the working directory, but Codex confines
neither reads nor commands, and `permissions.commands` and
`permissions.directories` are not turned into any setting. Every Codex `read`
and `write` result carries a note that starts `wider than requested`. See
[Permission mapping](../providers.md#permission-mapping-1),
[Wider than requested](../providers.md#wider-than-requested) and
[What cannot be enforced](../providers.md#what-cannot-be-enforced-1).

Where a provider does not do what the request asked, the result says so in
`provider_notes`.

## Strict

With `permissions.strict` (`--strict`), a request the provider cannot enforce
as stated is refused before it starts, with kind `permissions_not_enforceable`,
and the message names each part. Without it, the run goes ahead and a note says
what differs. `full` asks for nothing confined, so it is not refused for this.

With Codex, `strict` refuses every `read` and `write` request. With Claude Code
it refuses only `permissions.commands` when no command guard is configured. See
[Permissions](../api.md#permissions), Claude Code's
[Strict](../providers.md#strict) and Codex's
[Wider than requested](../providers.md#wider-than-requested).

## Requests that are not granted

`permissions.on_request` has one value today, `deny`: when the provider asks for
something not granted, it is answered no and recorded in the result's
`permissions.denied`. A run that was denied something and still returned a
final answer is `completed`. It is `refused` with kind `permission_denied` only
when the run stopped because of the denial; the rule is in
[Outcomes](../providers.md#outcomes). Codex reports denials only for commands
its stream marks `declined`
([What cannot be enforced](../providers.md#what-cannot-be-enforced-1)).

## Deny rules

A Claude Code worker loads none of the user's or the project's Claude Code
settings, except their `permissions.deny` rules. delega reads those rules from
the settings files and adds them to the worker, where they can only narrow the
run, also in `full`. A settings file that exists and cannot be used refuses the
run. Which files are read, and when one is skipped, is in
[Deny rules](../providers.md#deny-rules). Codex gets none of this.

## The environment a worker receives

A worker does not inherit the caller's environment. The policy is an allowlist:
a fixed base of variables, proxy and certificate variables, the provider's own
variables, the names the caller adds, and the variables the adapter sets.
Everything else is withheld. Variables that describe the calling session and
every `DELEGA_*` variable are never passed, even when named. The lists are in
[Environment policy](../providers.md#environment-policy).

To pass more:

- `--env NAME` (short `-e`, repeatable) passes the caller's value of one
  variable, and `--env NAME=VALUE` sets one. `delega mcp` takes the same flag
  and applies it to every worker it starts.
- The [configuration file](../api.md#configuration-file)'s `[env]` section adds
  names (`pass`) and values (`[env.set]`) with the same rules. Both apply; the
  file's come first, so a flag that sets the same variable wins.

A name that is not valid, a calling-session or `DELEGA_*` name, or a name
delega itself sets for the provider is refused, with kind `invalid_env_name` or
`forbidden_env_name`. [Command line](../api.md#command-line) says when each
surface checks. Results report the environment by names and counts, in a
`provider_notes` entry and in `applied.env` ([`applied`](../api.md#applied)),
not by values.

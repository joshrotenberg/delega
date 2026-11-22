# Install

This page builds delega from source, checks the machine with `delega doctor`
and shows how to depend on the library.

## Requirements

- macOS or Linux. Windows is not supported.
- Rust 1.90 or later.
- Claude Code, Codex or both, installed and logged in.

## Build from source

There is no release yet
([#140](https://github.com/joshrotenberg/delega/issues/140)), so build from
source:

```sh
git clone https://github.com/joshrotenberg/delega
cd delega
cargo install --locked --path crates/delega
```

This installs the `delega` executable into Cargo's bin directory. Check it is
on your `PATH`:

```sh
delega --version
```

## Check the machine

`delega doctor` reports what a run would meet, without starting a worker and
without writing anything:

```sh
delega doctor
delega doctor --output json
```

It reports, for each provider, the executable a run would use and where it came
from, its version, the environment and deny rules a worker would get, and the
managed settings found on the system. It also reports the configuration file,
the limits in force and whether process inspection works, and lists every
problem with what it blocks. The only provider process it starts is each
provider's `--version` probe. It never prints an environment value, a deny rule
or the content of a file.

doctor exits 0 when a run can go ahead.

A provider that is not installed is a problem for that provider and does not by
itself make the status 1 when the other provider is ready. The problem kinds and
the JSON report are described under [Doctor](../api.md#doctor), with the exit
statuses.

## Finding the providers

delega looks for `claude` and `codex` on the absolute entries of `PATH`.
`DELEGA_CLAUDE_BIN` and `DELEGA_CODEX_BIN` (absolute paths), or the executables
named in the configuration file, choose another one; the variables override the
file ([Finding the executable](../providers.md#finding-the-executable),
[Configuration file](../api.md#configuration-file)).

## Use the library

`delega-core` is not published to crates.io. Depend on it from git:

```toml
[dependencies]
delega-core = { git = "https://github.com/joshrotenberg/delega" }
```

One run, from the body of an async function on a Tokio runtime. The same lines
are the `no_run` doc test in
[`crates/delega-core/src/provider/mod.rs`](https://github.com/joshrotenberg/delega/blob/main/crates/delega-core/src/provider/mod.rs),
which `cargo test` compiles:

```rust,ignore
use delega_core::contract::request::{Provider, Request};
use delega_core::provider::{ProviderOptions, run};

let request = Request::new(Provider::Codex, "Summarize this repository");
let result = run(request, ProviderOptions::default()).await;
println!("{:?}", result.outcome);
```

Several workers at once go through `delega_core::supervisor::Supervisor`, whose
module documentation has the same kind of example. See
[The supervisor](../supervisor.md) and [Library](../api.md#library).

## Next

- [First run](first-run.md)
- [Plugin setup](plugin.md), to use delega from a Claude Code or Codex session.

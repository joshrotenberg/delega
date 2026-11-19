//! Run a request on Claude Code or Codex CLI as a child process.
//!
//! [`run`] takes a [`Request`](crate::contract::request::Request) and returns
//! exactly one [`RunResult`](crate::contract::result::RunResult). [`start`]
//! returns a [`RunHandle`] that can interrupt the run. [`dry_run`] reports what
//! would start without starting it. `docs/providers.md` describes the command
//! shape, the permission mapping and the environment policy of each provider.
//!
//! ```no_run
//! # async fn demo() {
//! use delega_core::contract::request::{Provider, Request};
//! use delega_core::provider::{ProviderOptions, run};
//!
//! let request = Request::new(Provider::Codex, "Summarize this repository");
//! let result = run(request, ProviderOptions::default()).await;
//! println!("{:?}", result.outcome);
//! # }
//! ```
//!
//! # Outcomes
//!
//! - `completed`: the provider reported success, exited 0 and gave an answer.
//! - `failed`: a provider error, a non-zero exit, a stream with no usable final
//!   result, an answer that is not JSON when `answer_schema` is set, or a
//!   preparation step that failed. A structured answer that parses but does
//!   not satisfy the schema is `completed` with `structured_valid: false`.
//! - `timed_out` and `interrupted`: the process layer stopped the provider.
//! - `refused` before start: an invalid request, an unusable working directory
//!   or directory, a rejected native argument, a missing provider executable,
//!   an unsupported provider version for exact command grants.
//! - `refused` after start: the provider reported failure and at least one
//!   request had been denied.
//!
//! # Platform
//!
//! Unix only, like the process layer.

#![cfg(unix)]

mod claude;
mod codex;
mod dry_run;
pub mod env;
mod facts;
pub mod hook;
mod launch;
pub mod limits;
pub mod native_args;
mod options;
mod permissions;
mod plan;
pub mod program;
mod prompt;
mod refusal;
mod run;
mod settle;
mod stream;
mod structured;

#[cfg(test)]
mod tests;

pub use dry_run::{DryRun, dry_run};
pub use hook::HookCommand;
pub use options::ProviderOptions;
pub use permissions::Mapping;
pub use refusal::Refusal;
pub use run::{RunHandle, RunInterrupter, run, start};

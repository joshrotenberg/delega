//! What the caller of the provider layer may set besides the request.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::contract::result::WorkerRef;

use super::hook::HookCommand;
use super::progress::ProgressSink;

/// Settings for one provider run that are not part of the request.
///
/// Every field has a default, so `ProviderOptions::default()` is the normal
/// configuration. Debug output lists environment variable names only.
#[derive(Clone, Default)]
pub struct ProviderOptions {
    /// The provider executable, an absolute path. Without it the lookup uses
    /// `DELEGA_CLAUDE_BIN` or `DELEGA_CODEX_BIN` from the parent environment,
    /// then the provider's name on the absolute entries of `PATH`.
    pub executable: Option<PathBuf>,
    /// The command that runs the exact command guard for Claude Code. Without
    /// it, `permissions.commands` is not granted to a Claude worker and the
    /// result says so. See [`super::hook`].
    pub hook: Option<HookCommand>,
    /// Names of variables of the parent environment to pass although the
    /// allowlist does not cover them, such as a cloud credential or a
    /// toolchain home. A name that is not set in the parent is ignored. A name
    /// that is not a valid variable name, that describes the calling session or
    /// that starts with `DELEGA_` is refused before start.
    pub env_pass: Vec<String>,
    /// Variables to set for the provider, after the policy has run. A name the
    /// adapter owns cannot be overridden. A name that is not valid, that
    /// describes the calling session or that starts with `DELEGA_` is refused
    /// before start, as for `env_pass`.
    pub env_set: Vec<(String, String)>,
    /// The environment to inherit from. `None` means this process's
    /// environment. Tests set it to stay independent of the real one.
    pub parent_env: Option<Vec<(OsString, OsString)>>,
    /// The run id to report. One is generated when absent.
    pub run_id: Option<String>,
    /// The worker to report in the result, when an engine owns one.
    pub worker: Option<WorkerRef>,
    /// Most bytes of provider standard output kept. Defaults to
    /// [`limits::MAX_OUTPUT_BYTES`](super::limits::MAX_OUTPUT_BYTES).
    pub max_output_bytes: Option<usize>,
    /// Grace between SIGTERM and SIGKILL. Defaults to the process layer's.
    pub stop_grace: Option<Duration>,
    /// Receives [`ProgressEvent`](super::ProgressEvent)s while the run goes
    /// on. Without it nothing is reported live.
    pub progress: Option<ProgressSink>,
}

impl ProviderOptions {
    /// The environment to inherit from.
    pub(super) fn parent_env(&self) -> Vec<(OsString, OsString)> {
        match &self.parent_env {
            Some(env) => env.clone(),
            None => std::env::vars_os().collect(),
        }
    }
}

impl fmt::Debug for ProviderOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set_names: Vec<&str> = self.env_set.iter().map(|(n, _)| n.as_str()).collect();
        let parent_names: Option<Vec<String>> = self.parent_env.as_ref().map(|env| {
            env.iter()
                .map(|(n, _)| n.to_string_lossy().into_owned())
                .collect()
        });
        f.debug_struct("ProviderOptions")
            .field("executable", &self.executable)
            .field("hook", &self.hook)
            .field("env_pass", &self.env_pass)
            .field("env_set_names", &set_names)
            .field("parent_env_names", &parent_names)
            .field("run_id", &self.run_id)
            .field("worker", &self.worker)
            .field("max_output_bytes", &self.max_output_bytes)
            .field("stop_grace", &self.stop_grace)
            .field("progress", &self.progress.is_some())
            .finish()
    }
}

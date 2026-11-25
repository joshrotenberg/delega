//! What the caller of the provider layer may set besides the request.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::contract::applied::Applied;
use crate::contract::request::Provider;
use crate::contract::result::WorkerRef;

use super::hook::HookCommand;
use super::managed::SystemPaths;
use super::progress::ProgressSink;

/// Settings for one provider run that are not part of the request.
///
/// Every field has a default, so `ProviderOptions::default()` is the normal
/// configuration. Debug output lists environment variable names only.
#[derive(Clone, Default)]
pub struct ProviderOptions {
    /// Where to look for managed settings. Defaults to the current platform's
    /// system locations under `/`.
    pub system_paths: SystemPaths,
    /// The provider executable, an absolute path. Without it the lookup uses
    /// `DELEGA_CLAUDE_BIN` or `DELEGA_CODEX_BIN` from the parent environment,
    /// then the provider's name on the absolute entries of `PATH`.
    pub executable: Option<PathBuf>,
    /// The Claude Code executable from a configuration file, an absolute path.
    /// Used when `executable` is absent and `DELEGA_CLAUDE_BIN` is unset or
    /// empty; it comes before the search of `PATH`.
    pub claude_executable: Option<PathBuf>,
    /// The Codex executable from a configuration file; as `claude_executable`,
    /// with `DELEGA_CODEX_BIN`.
    pub codex_executable: Option<PathBuf>,
    /// The command that runs the exact command guard for Claude Code. Without
    /// it, `permissions.commands` is not granted to a Claude worker and the
    /// result says so. See [`super::hook`].
    pub hook: Option<HookCommand>,
    /// The command that runs the keeper (see
    /// [`KeeperCommand`](crate::process::KeeperCommand)): the process between
    /// delega and the provider that stops the provider's whole tree when
    /// delega is gone and, on Linux, adopts every orphaned descendant. Without
    /// it the provider is delega's own child, the gaps in the process module's
    /// documentation remain, and the result says so in `provider_notes`.
    pub keeper: Option<crate::process::KeeperCommand>,
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
    /// The parts of a result's `applied` that only the caller knows: `config`,
    /// the configuration file it read, and `sources`, which layer set each
    /// request field. The other parts are filled in from the prepared run, and
    /// anything set here for them is replaced. Empty by default, which reports
    /// nothing about the caller's layers.
    pub origin: Applied,
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
    /// Also send each agent message's text, with credentials removed and cut to
    /// [`limits::PROGRESS_TEXT_CHARS`](super::limits::PROGRESS_TEXT_CHARS), as
    /// [`ProgressEvent::Message`](super::ProgressEvent::Message). Off by
    /// default, so a sink sees message lengths only. Needs `progress`.
    pub progress_text: bool,
    /// Internal endpoint installed by the supervisor.
    #[doc(hidden)]
    pub asks: super::ask::AskEndpoint,
    /// Test seam: chunks queued for the progress tap, in place of the process
    /// layer's, so a test decides how many chunks are dropped.
    #[cfg(test)]
    pub(crate) tap_queue_chunks: Option<usize>,
    /// Test seam: the run-marker list a kept run would inherit, in place of
    /// this process's own, so a test need not change its environment.
    #[cfg(test)]
    pub(crate) inherited_marker: Option<OsString>,
}

impl ProviderOptions {
    /// The executable a configuration file names for `provider`.
    pub(super) fn configured_executable(&self, provider: Provider) -> Option<&PathBuf> {
        match provider {
            Provider::Claude => self.claude_executable.as_ref(),
            Provider::Codex => self.codex_executable.as_ref(),
        }
    }

    /// The run-marker list a kept run extends: this process's own (see
    /// [`crate::process::MARKER_ENV`]), which the process layer reads.
    pub(super) fn inherited_marker(&self) -> Option<OsString> {
        #[cfg(test)]
        if self.inherited_marker.is_some() {
            return self.inherited_marker.clone();
        }
        crate::process::inherited_marker()
    }

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
            .field("system_paths", &self.system_paths)
            .field("executable", &self.executable)
            .field("claude_executable", &self.claude_executable)
            .field("codex_executable", &self.codex_executable)
            .field("hook", &self.hook)
            .field("keeper", &self.keeper)
            .field("env_pass", &self.env_pass)
            .field("env_set_names", &set_names)
            .field("parent_env_names", &parent_names)
            .field("origin", &self.origin)
            .field("run_id", &self.run_id)
            .field("worker", &self.worker)
            .field("max_output_bytes", &self.max_output_bytes)
            .field("stop_grace", &self.stop_grace)
            .field("progress", &self.progress.is_some())
            .field("progress_text", &self.progress_text)
            .finish()
    }
}

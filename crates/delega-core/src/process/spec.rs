//! What to start: a neutral description of one child process.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use super::limits::{
    CLEANUP_DEADLINE_MARGIN, DEFAULT_CLEANUP_DEADLINE, DEFAULT_DRAIN_BOUND,
    DEFAULT_STDERR_CAPTURE_BYTES, DEFAULT_STDOUT_CAPTURE_BYTES, DEFAULT_STOP_GRACE,
};
use super::tree::Lister;

/// Which environment variables the child sees.
///
/// The child's environment is the inherited part, chosen by the policy, with
/// every `set` entry applied on top and every `remove` name taken out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvPolicy {
    inherit: Inherit,
    set: BTreeMap<OsString, OsString>,
    remove: Vec<OsString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Inherit {
    All,
    Nothing,
    Only(Vec<OsString>),
}

impl EnvPolicy {
    /// Start from this process's whole environment.
    pub fn inherit_all() -> Self {
        Self::with(Inherit::All)
    }

    /// Start from an empty environment.
    pub fn empty() -> Self {
        Self::with(Inherit::Nothing)
    }

    /// Start from this process's environment restricted to the named variables.
    pub fn inherit_only<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self::with(Inherit::Only(names.into_iter().map(Into::into).collect()))
    }

    fn with(inherit: Inherit) -> Self {
        Self {
            inherit,
            set: BTreeMap::new(),
            remove: Vec::new(),
        }
    }

    /// Set a variable, replacing any inherited or earlier value.
    #[must_use]
    pub fn set(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.set.insert(name.into(), value.into());
        self
    }

    /// Remove a variable that would otherwise be inherited.
    #[must_use]
    pub fn remove(mut self, name: impl Into<OsString>) -> Self {
        self.remove.push(name.into());
        self
    }

    /// The environment the child is given, resolved against `parent`.
    pub(crate) fn resolve(
        &self,
        parent: impl Iterator<Item = (OsString, OsString)>,
    ) -> Vec<(OsString, OsString)> {
        let mut env: BTreeMap<OsString, OsString> = match &self.inherit {
            Inherit::All => parent.collect(),
            Inherit::Nothing => BTreeMap::new(),
            Inherit::Only(names) => parent.filter(|(name, _)| names.contains(name)).collect(),
        };
        for name in &self.remove {
            env.remove(name);
        }
        env.extend(self.set.clone());
        env.into_iter().collect()
    }
}

/// Which end of a stream is kept when it exceeds its bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// Keep the first bytes and discard the rest.
    Head,
    /// Keep the last bytes and discard the earlier ones.
    Tail,
}

/// How much of one output stream to keep. The stream is always read to its
/// end so the child never blocks on a full pipe; only what is kept is bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureSpec {
    /// The most bytes kept.
    pub limit: usize,
    /// Which end is kept past the limit.
    pub keep: Keep,
}

/// A function called with chunks read from the child's standard output, in
/// order, while the child runs. It runs on a thread of its own and is fed
/// through a bounded queue: a tap that is slow loses chunks (the report's
/// warnings count them) and a tap that panics receives nothing more, but
/// neither delays the run, its time limit or the capture.
pub type OutputTap = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// A child process to run.
#[derive(Clone)]
pub struct ProcessSpec {
    /// The program to execute, looked up as `std::process::Command` does, in
    /// the parent's `PATH` when it has no slash.
    pub program: OsString,
    /// Arguments after the program name.
    pub args: Vec<OsString>,
    /// The environment policy.
    pub env: EnvPolicy,
    /// The working directory. The parent's when absent.
    pub cwd: Option<PathBuf>,
    /// Bytes written to the child's standard input, which is then closed. With
    /// `None` the child's standard input is the null device.
    pub stdin: Option<Vec<u8>>,
    /// How long the child may run before it is stopped and the ending is
    /// [`Ending::TimedOut`](super::Ending::TimedOut).
    pub time_limit: Duration,
    /// Standard output capture.
    pub stdout: CaptureSpec,
    /// Standard error capture.
    pub stderr: CaptureSpec,
    /// Called with every chunk of standard output as it is read, in addition
    /// to the capture. Chunks have no line framing.
    pub stdout_tap: Option<OutputTap>,
    /// Incremented by one for each chunk the tap was not given because it was
    /// too slow. Lets the caller report drops, which the report's warnings
    /// carry only as text.
    pub stdout_tap_dropped: Option<Arc<AtomicU64>>,
    /// Grace between SIGTERM and SIGKILL.
    pub stop_grace: Duration,
    /// How long output pipes may stay open after the kill.
    pub drain_bound: Duration,
    /// How long cleanup keeps signalling, from the first stop signal, before
    /// it reports what remains. A value below `stop_grace + drain_bound` plus
    /// [`CLEANUP_DEADLINE_MARGIN`](super::limits::CLEANUP_DEADLINE_MARGIN) is
    /// raised to that sum when the run starts, and the report carries a
    /// warning saying so.
    pub cleanup_deadline: Duration,
    /// Whether to track descendants with `ps`. Without it only the child's own
    /// process group is signalled.
    pub track_descendants: bool,
    pub(crate) lister: Option<Lister>,
    /// Test seam: make the first N group SIGKILLs do nothing.
    #[cfg(test)]
    pub(crate) swallow_group_kills: u32,
    /// Test seam: panic in the owner loop right after the child starts.
    #[cfg(test)]
    pub(crate) panic_in_loop: bool,
    /// Test seam: the interval between periodic scans, in place of
    /// `limits::CAPTURE_INTERVAL`, so a test can rule a periodic scan out.
    #[cfg(test)]
    pub(crate) capture_interval: Option<Duration>,
    /// Test seam: the waits that depend on how fast `ps` answers. See
    /// [`TestTimings`].
    #[cfg(test)]
    pub(crate) timings: TestTimings,
}

/// Test seam. The production waits for a process listing (`limits`) are tuned
/// for a machine at ordinary load, and a test that expects a listing to finish
/// in time fails when the machine is busy. In a test build every spec starts
/// with waits far longer than any listing, so a test about what happens when
/// a listing is late says so explicitly (a short `stop_listing`, or a lister
/// that waits for a gate) and every other test is independent of speed.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct TestTimings {
    pub(crate) stop_listing: Duration,
    pub(crate) exit_listing: Duration,
    pub(crate) final_cleanup_pass: Duration,
    pub(crate) descendant_signal: Duration,
    pub(crate) tap_queue_chunks: usize,
}

#[cfg(test)]
impl Default for TestTimings {
    fn default() -> Self {
        let long = Duration::from_secs(120);
        Self {
            stop_listing: long,
            exit_listing: long,
            final_cleanup_pass: long,
            descendant_signal: long,
            tap_queue_chunks: super::limits::TAP_QUEUE_CHUNKS,
        }
    }
}

impl ProcessSpec {
    /// A spec that runs `program` with no arguments, the parent's environment
    /// and no standard input, under `time_limit`. Capture bounds and stop
    /// timings take their defaults from [`limits`](super::limits).
    pub fn new(program: impl Into<OsString>, time_limit: Duration) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: EnvPolicy::inherit_all(),
            cwd: None,
            stdin: None,
            time_limit,
            stdout: CaptureSpec {
                limit: DEFAULT_STDOUT_CAPTURE_BYTES,
                keep: Keep::Head,
            },
            stderr: CaptureSpec {
                limit: DEFAULT_STDERR_CAPTURE_BYTES,
                keep: Keep::Tail,
            },
            stdout_tap: None,
            stdout_tap_dropped: None,
            stop_grace: DEFAULT_STOP_GRACE,
            drain_bound: DEFAULT_DRAIN_BOUND,
            cleanup_deadline: DEFAULT_CLEANUP_DEADLINE,
            track_descendants: true,
            lister: None,
            #[cfg(test)]
            swallow_group_kills: 0,
            #[cfg(test)]
            panic_in_loop: false,
            #[cfg(test)]
            capture_interval: None,
            #[cfg(test)]
            timings: TestTimings::default(),
        }
    }

    /// How long a requested stop waits for the listing before its first signal.
    #[cfg(test)]
    pub(crate) fn stop_listing_budget(&self) -> Duration {
        self.timings.stop_listing
    }

    /// How long a requested stop waits for the listing before its first signal.
    #[cfg(not(test))]
    pub(crate) fn stop_listing_budget(&self) -> Duration {
        super::limits::STOP_LISTING_BUDGET
    }

    /// How long the first signal after a natural exit waits for its listing.
    #[cfg(test)]
    pub(crate) fn exit_listing_budget(&self) -> Duration {
        self.timings.exit_listing
    }

    /// How long the first signal after a natural exit waits for its listing.
    #[cfg(not(test))]
    pub(crate) fn exit_listing_budget(&self) -> Duration {
        super::limits::EXIT_LISTING_BUDGET
    }

    /// Least time the last cleanup pass gets.
    #[cfg(test)]
    pub(crate) fn final_cleanup_pass(&self) -> Duration {
        self.timings.final_cleanup_pass
    }

    /// Least time the last cleanup pass gets.
    #[cfg(not(test))]
    pub(crate) fn final_cleanup_pass(&self) -> Duration {
        super::limits::FINAL_CLEANUP_PASS
    }

    /// Chunks queued for the stdout tap.
    #[cfg(test)]
    pub(crate) fn tap_queue_chunks(&self) -> usize {
        self.timings.tap_queue_chunks
    }

    /// Chunks queued for the stdout tap.
    #[cfg(not(test))]
    pub(crate) fn tap_queue_chunks(&self) -> usize {
        super::limits::TAP_QUEUE_CHUNKS
    }

    /// The cleanup deadline in force: `cleanup_deadline`, raised to
    /// `stop_grace + drain_bound` plus the margin when it is smaller.
    pub(crate) fn effective_cleanup_deadline(&self) -> Duration {
        let least = self.stop_grace + self.drain_bound + CLEANUP_DEADLINE_MARGIN;
        self.cleanup_deadline.max(least)
    }

    /// Set the arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Set the environment policy.
    #[must_use]
    pub fn env(mut self, env: EnvPolicy) -> Self {
        self.env = env;
        self
    }

    /// Set the working directory.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Set the standard input content.
    #[must_use]
    pub fn stdin(mut self, content: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(content.into());
        self
    }

    /// Replace the process lister, so a test controls what the tracker sees.
    #[cfg(test)]
    pub(crate) fn with_lister(mut self, lister: Lister) -> Self {
        self.lister = Some(lister);
        self
    }
}

impl std::fmt::Debug for ProcessSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env", &self.env)
            .field("cwd", &self.cwd)
            .field("stdin_bytes", &self.stdin.as_ref().map(Vec::len))
            .field("time_limit", &self.time_limit)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .field("stdout_tap", &self.stdout_tap.is_some())
            .field("stop_grace", &self.stop_grace)
            .field("drain_bound", &self.drain_bound)
            .field("cleanup_deadline", &self.cleanup_deadline)
            .field("track_descendants", &self.track_descendants)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cleanup_deadline_shorter_than_the_stop_sequence_is_raised() {
        let mut spec = ProcessSpec::new("x", Duration::from_secs(1));
        spec.stop_grace = Duration::from_secs(2);
        spec.drain_bound = Duration::from_secs(3);
        spec.cleanup_deadline = Duration::from_secs(1);
        assert_eq!(
            spec.effective_cleanup_deadline(),
            Duration::from_secs(5) + CLEANUP_DEADLINE_MARGIN
        );
        spec.cleanup_deadline = Duration::from_secs(60);
        assert_eq!(spec.effective_cleanup_deadline(), Duration::from_secs(60));
    }

    fn parent() -> Vec<(OsString, OsString)> {
        vec![
            ("PATH".into(), "/bin".into()),
            ("HOME".into(), "/home/x".into()),
            ("SECRET".into(), "s".into()),
        ]
    }

    fn names(env: &[(OsString, OsString)]) -> Vec<&str> {
        env.iter().map(|(k, _)| k.to_str().unwrap()).collect()
    }

    #[test]
    fn empty_policy_passes_only_what_is_set() {
        let env = EnvPolicy::empty()
            .set("A", "1")
            .resolve(parent().into_iter());
        assert_eq!(names(&env), ["A"]);
    }

    #[test]
    fn inherit_only_keeps_named_variables_and_set_wins() {
        let env = EnvPolicy::inherit_only(["PATH", "HOME"])
            .set("HOME", "/other")
            .resolve(parent().into_iter());
        assert_eq!(names(&env), ["HOME", "PATH"]);
        assert_eq!(env[0].1, "/other");
    }

    #[test]
    fn inherit_all_with_remove_drops_the_named_variable() {
        let env = EnvPolicy::inherit_all()
            .remove("SECRET")
            .resolve(parent().into_iter());
        assert_eq!(names(&env), ["HOME", "PATH"]);
    }
}

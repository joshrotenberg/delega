//! Every numeric tunable of the provider layer.
//!
//! Counts, sizes and durations are named here so a change is one line and the
//! tests and `docs/providers.md` can refer to them.

use std::time::Duration;

/// The longest instruction-bearing launch argument, in bytes. Instructions that
/// do not fit are sent on standard input instead, and a note says so.
pub const ARGUMENT_LIMIT: usize = 120 * 1024;
/// The longest generated Claude `--settings` argument, in bytes.
pub const SETTINGS_ARGUMENT_LIMIT: usize = 120 * 1024;

/// The largest Claude Code settings file read for deny rules, in bytes. A larger
/// file, or one that is not a regular file, refuses the run.
pub const SETTINGS_FILE_BYTES: usize = 1024 * 1024;
/// Most deny rules accepted from one settings file.
pub const MAX_DENY_RULES_PER_FILE: usize = 256;
/// Most deny rules passed to a worker after deduplication and adding absolute forms.
pub const MAX_DENY_RULES: usize = 512;
/// The longest single deny rule, in characters.
pub const MAX_DENY_RULE_CHARS: usize = 512;
/// Most deny rules a dry run prints. The rest are counted.
pub const DRY_RUN_DENY_RULES_SHOWN: usize = 100;
/// The longest deny rule a dry run prints, in characters.
pub const DRY_RUN_DENY_RULE_CHARS: usize = 200;

/// How long the `--version` probe may run.
pub const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the Codex MCP discovery probe may run.
pub const MCP_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Most bytes kept from a probe's standard output.
pub const PROBE_STDOUT_BYTES: usize = 2_000_000;
/// Most bytes kept from a probe's standard error.
pub const PROBE_STDERR_BYTES: usize = 16 * 1024;
/// Grace between SIGTERM and SIGKILL for a probe.
pub const PROBE_STOP_GRACE: Duration = Duration::from_millis(500);

/// The longest provider output line that is parsed, in bytes. A longer line is
/// counted and skipped.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// Most bytes of provider standard output kept, unless the options set another
/// bound. It equals the process layer's default.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Most bytes of provider standard error kept.
pub const MAX_STDERR_BYTES: usize = 64 * 1024;

/// Longest provider stderr excerpt put in an error message, in characters.
pub const STDERR_EXCERPT_CHARS: usize = 300;
/// Longest error message taken from a provider event, in characters.
pub const PROVIDER_MESSAGE_CHARS: usize = 500;
/// Longest target (command, path) kept in a denied request, in characters.
pub const DENIAL_TARGET_CHARS: usize = 500;
/// Most denied requests recorded. Later ones are counted in a note.
pub const MAX_DENIED: usize = 64;
/// Longest provider version text kept, in characters.
pub const VERSION_CHARS: usize = 100;
/// Longest schema validation message kept, in characters.
pub const VALIDATION_MESSAGE_CHARS: usize = 300;

/// The oldest Claude Code release that supports the blocking hook interface the
/// exact command guard needs.
pub const MIN_CLAUDE_HOOK_VERSION: (u64, u64, u64) = (2, 1, 325);
/// The most standard input the command guard reads, in bytes.
pub const HOOK_INPUT_LIMIT: usize = 2_000_000;
/// The guard's own time limit as the provider enforces it, in seconds.
pub const HOOK_TIMEOUT_SECS: u64 = 5;

/// The longest accepted `native_args` value that is a name, in characters.
pub const NATIVE_NAME_CHARS: usize = 200;
/// The longest accepted numeric `native_args` value, in characters.
pub const NATIVE_NUMBER_CHARS: usize = 10;

/// Longest denial reason kept after a denied request, in characters.
pub const DENIAL_REASON_CHARS: usize = 300;
/// Most tool error reasons remembered while reading a Claude Code stream.
pub const MAX_REASONS: usize = 64;
/// Most bytes of provider standard error read for an error message, from the
/// end.
pub const STDERR_SCAN_BYTES: usize = 8 * 1024;
/// Shortest environment value that is removed from provider text. Shorter
/// values would also remove ordinary words.
pub const MIN_REDACTED_VALUE_CHARS: usize = 6;
/// Longest prefix of a rejected `native_args` option named in a message.
pub const NATIVE_FLAG_NAME_CHARS: usize = 40;

/// Longest message text in a progress event summary, in characters.
pub const PROGRESS_SUMMARY_CHARS: usize = 80;

/// Longest agent message text in a [`ProgressEvent::Message`](super::ProgressEvent::Message),
/// in characters. Sent only when
/// [`ProviderOptions::progress_text`](super::ProviderOptions::progress_text) is set.
pub const PROGRESS_TEXT_CHARS: usize = 4_000;

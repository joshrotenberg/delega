//! Numeric limits applied when a request is validated.
//!
//! Every bound lives here so a change to one is a change to one line, and so
//! the validation tests and the documentation can name them.

/// The timeout applied when a request does not set one: 10 minutes.
pub const DEFAULT_TIMEOUT_SECS: u64 = 10 * 60;
/// The shortest accepted timeout.
pub const MIN_TIMEOUT_SECS: u64 = 1;
/// The longest accepted timeout: 24 hours.
pub const MAX_TIMEOUT_SECS: u64 = 24 * 60 * 60;

/// The longest accepted prompt, in bytes.
pub const MAX_PROMPT_BYTES: usize = 4 * 1024 * 1024;
/// The longest accepted `instructions` text, in bytes.
pub const MAX_INSTRUCTIONS_BYTES: usize = 256 * 1024;
/// The longest accepted worker name, in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// The longest accepted model name or session id, in bytes.
pub const MAX_IDENTIFIER_BYTES: usize = 256;

/// The most entries accepted in `permissions.commands`.
pub const MAX_COMMANDS: usize = 256;
/// The longest accepted single command string, in bytes.
pub const MAX_COMMAND_BYTES: usize = 4096;
/// The most entries accepted in `permissions.directories`.
pub const MAX_DIRECTORIES: usize = 64;
/// The most entries accepted in `native_args`.
pub const MAX_NATIVE_ARGS: usize = 64;
/// The longest accepted single `native_args` entry, in bytes.
pub const MAX_NATIVE_ARG_BYTES: usize = 4096;

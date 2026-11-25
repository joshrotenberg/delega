//! The per-argument size check on what delega generates for a provider launch.
//!
//! On Linux every single argument and every single environment entry
//! (`NAME=value`), with its terminating NUL, must fit in `MAX_ARG_STRLEN`, 32
//! pages (128 KiB with 4 KiB pages), or `exec` fails with `E2BIG`. delega
//! checks each one it generates against [`ARGUMENT_LIMIT`] before anything
//! starts, and refuses with [`TOO_LONG`] rather than let the run fail to
//! start. A kept run also adds the run marker to every process it starts, so
//! that entry is checked too.

use super::limits::ARGUMENT_LIMIT;
use super::refusal::Refusal;
use crate::process::{MARKER_ENV, Marker};

/// The refusal kind for a generated argument or environment entry over
/// [`ARGUMENT_LIMIT`].
pub(super) const TOO_LONG: &str = "argument_too_long";

/// Refuse when the argument or environment entry `what` is `len` bytes, over
/// [`ARGUMENT_LIMIT`]. `what` names it and never holds its content.
pub(super) fn check(what: &str, len: usize) -> Result<(), Refusal> {
    check_with(
        what,
        len,
        "use fewer or shorter permissions.directories, permissions.commands or other inputs \
         it is built from",
    )
}

/// [`check`] with the advice the message ends with.
fn check_with(what: &str, len: usize, advice: &str) -> Result<(), Refusal> {
    if len <= ARGUMENT_LIMIT {
        return Ok(());
    }
    Err(Refusal::new(
        TOO_LONG,
        format!(
            "{what} would be {len} bytes, over the {ARGUMENT_LIMIT} byte limit for one launch \
             argument or environment entry; {advice}"
        ),
    ))
}

/// Refuse when the run marker a kept run adds to every process it starts
/// would be over [`ARGUMENT_LIMIT`]. It extends the list `inherited` from this
/// process's environment, so nested kept runs lengthen it.
pub(super) fn check_marker(inherited: Option<&std::ffi::OsStr>) -> Result<(), Refusal> {
    check_with(
        &format!("the environment entry {MARKER_ENV}"),
        Marker::entry_len(inherited),
        &format!(
            "it extends the {MARKER_ENV} list this process inherited, which each nested kept \
             run lengthens; start delega from an environment without it"
        ),
    )
}

/// Refuse when any launch argument is over [`ARGUMENT_LIMIT`].
pub(super) fn check_args(args: &[String]) -> Result<(), Refusal> {
    for (index, arg) in args.iter().enumerate() {
        if arg.len() > ARGUMENT_LIMIT {
            check(&describe(args, index), arg.len())?;
        }
    }
    Ok(())
}

/// Refuse when any environment entry delega sets for the provider, measured
/// as `NAME=value`, is over [`ARGUMENT_LIMIT`].
pub(super) fn check_env(env: &[(String, String)]) -> Result<(), Refusal> {
    for (name, value) in env {
        check(
            &format!("the environment entry {}", safe_name(name)),
            name.len() + 1 + value.len(),
        )?;
    }
    Ok(())
}

/// A name for the argument at `index` that holds none of its content: the
/// option it is the value of, with the key of a `-c key=value` setting.
pub(super) fn describe(args: &[String], index: usize) -> String {
    let flag = index
        .checked_sub(1)
        .map(|i| args[i].as_str())
        .filter(|a| is_flag(a));
    match flag {
        Some(flag @ ("-c" | "--config")) => {
            match args[index]
                .split_once('=')
                .map(|(k, _)| k)
                .filter(|k| is_key(k))
            {
                Some(key) => format!("the {flag} {key} argument"),
                None => format!("the {flag} argument at position {index}"),
            }
        }
        Some(flag) => format!("the {flag} argument"),
        None => format!("the launch argument at position {index}"),
    }
}

/// Whether `text` is an option name: one or two hyphens, then a letter, then
/// letters, digits and hyphens, short enough to quote.
fn is_flag(text: &str) -> bool {
    let rest = text
        .strip_prefix("--")
        .or_else(|| text.strip_prefix('-'))
        .unwrap_or_default();
    rest.len() <= 40
        && rest.starts_with(|c: char| c.is_ascii_alphabetic())
        && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Whether `text` is a dotted configuration key, short enough to quote.
fn is_key(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// `name` when it is a plain variable name, or a placeholder.
fn safe_name(name: &str) -> &str {
    if is_key(name) && !name.contains('.') {
        name
    } else {
        "with an unusual name"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn an_argument_exactly_at_the_limit_is_accepted_and_one_byte_more_is_refused() {
        let at = args(&["--allowedTools", &"x".repeat(ARGUMENT_LIMIT)]);
        assert_eq!(check_args(&at), Ok(()));
        let over = args(&["--allowedTools", &"x".repeat(ARGUMENT_LIMIT + 1)]);
        let refusal = check_args(&over).expect_err("over the limit");
        assert_eq!(refusal.kind, TOO_LONG);
        assert!(refusal.message.contains("--allowedTools"), "{refusal}");
        assert!(
            refusal
                .message
                .contains(&format!("{} bytes", ARGUMENT_LIMIT + 1)),
            "{refusal}"
        );
        assert!(!refusal.message.contains("xxxx"), "{refusal}");
    }

    #[test]
    fn an_environment_entry_counts_its_name_and_equals_sign_against_the_limit() {
        let name = crate::provider::hook::GRANTS_ENV.to_string();
        let fits = "v".repeat(ARGUMENT_LIMIT - name.len() - 1);
        assert_eq!(check_env(&[(name.clone(), fits.clone())]), Ok(()));
        let refusal = check_env(&[(name.clone(), format!("{fits}v"))]).expect_err("over");
        assert_eq!(refusal.kind, TOO_LONG);
        assert!(refusal.message.contains(&name), "{refusal}");
        assert!(!refusal.message.contains("vvvv"), "{refusal}");
    }

    #[test]
    fn a_codex_setting_is_named_by_its_key_and_never_by_its_value() {
        let list = args(&["-c", "mcp_servers={\"secret-name\"=1}"]);
        assert_eq!(describe(&list, 1), "the -c mcp_servers argument");
        let list = args(&["-c", "not a key=secret"]);
        assert_eq!(describe(&list, 1), "the -c argument at position 1");
        let list = args(&["secret text", "more secret"]);
        assert_eq!(describe(&list, 1), "the launch argument at position 1");
    }
}

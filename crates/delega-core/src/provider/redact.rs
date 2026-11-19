//! Removing secrets from provider text before it goes into a result.
//!
//! Provider standard error and error messages can echo what the provider was
//! given. Two things are removed: the values of the environment variables the
//! provider was started with when the variable is secret-looking or belongs to
//! the provider, and strings with the shape of a common credential (API keys,
//! cloud and forge tokens, JSON web tokens, bearer tokens, private key blocks).
//! The shapes are a heuristic, so a credential in an unusual form is not caught.

use super::limits::MIN_REDACTED_VALUE_CHARS;

const MASK: &str = "[redacted]";

/// What to remove.
#[derive(Clone, Default)]
pub(super) struct Redactor {
    values: Vec<String>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor")
            .field("values", &self.values.len())
            .finish()
    }
}

/// Characters that can be inside a credential. Everything else, `=`, `:`, `/`,
/// `@`, quotes and commas included, separates tokens, so `token=ghp_...`,
/// `https://user:ghp_...@host` and `"key":"sk-..."` are all found.
fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_-.~+".contains(c)
}

fn all_alnum(text: &str, min: usize) -> bool {
    text.len() >= min && text.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Whether a token has the shape of a credential.
fn credential_shaped(token: &str) -> bool {
    let tail_ok = |rest: &str, min: usize| {
        rest.len() >= min
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if let Some(rest) = token.strip_prefix("sk-ant-") {
        return tail_ok(rest, 16);
    }
    if let Some(rest) = token.strip_prefix("sk-") {
        return tail_ok(
            rest.trim_start_matches("proj-")
                .trim_start_matches("svcacct-"),
            20,
        );
    }
    if let Some(rest) = token.strip_prefix("github_pat_") {
        return tail_ok(rest, 20);
    }
    for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
        if let Some(rest) = token.strip_prefix(prefix) {
            return all_alnum(rest, 20);
        }
    }
    if let Some(rest) = token.strip_prefix("xox")
        && rest.len() > 2
        && "abposr".contains(&rest[..1])
        && rest.as_bytes()[1] == b'-'
    {
        return rest[2..].len() >= 10;
    }
    for prefix in ["AKIA", "ASIA"] {
        if let Some(rest) = token.strip_prefix(prefix) {
            return rest.len() == 16
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        }
    }
    if token.starts_with("eyJ") {
        let parts: Vec<&str> = token.split('.').collect();
        return parts.len() == 3
            && parts[0].len() >= 8
            && parts[1].len() >= 8
            && parts[2].len() >= 8
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.');
    }
    false
}

/// The longest label looked at after `-----BEGIN `, in bytes.
const BEGIN_LABEL_WINDOW: usize = 64;

// Test builds count the bytes the private key scan looks at, so a test can
// bound the work by the length of the input instead of timing it. Other
// builds compile the call away.
#[cfg(test)]
thread_local! {
    static EXAMINED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn examined(bytes: usize) {
    EXAMINED.with(|total| total.set(total.get() + bytes));
}

#[cfg(not(test))]
#[inline(always)]
fn examined(_bytes: usize) {}

/// The bytes the private key scan has looked at on this thread since the last
/// call, then reset. Counted at each place it searches: up to the marker the
/// search found (or the whole rest when it found none), the label window, and
/// up to the end line of a key body.
#[cfg(test)]
pub(super) fn take_examined() -> usize {
    EXAMINED.with(|total| total.replace(0))
}

/// Replace private key blocks, up to their end line or the end of the text.
/// One forward pass: every search starts where the last one stopped, and a
/// begin marker that is not a key is skipped after a bounded look.
fn without_private_keys(text: &str) -> String {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "PRIVATE KEY-----";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        examined(start + BEGIN.len());
        let after = &rest[start + BEGIN.len()..];
        let window = &after.as_bytes()[..after.len().min(BEGIN_LABEL_WINDOW)];
        examined(window.len());
        let label_end = window.windows(5).position(|w| w == b"-----");
        let is_key = label_end.is_some_and(|i| after.as_bytes()[..i].ends_with(b"PRIVATE KEY"));
        if !is_key {
            out.push_str(&rest[..start + BEGIN.len()]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..start]);
        out.push_str(MASK);
        let body = &after[label_end.unwrap_or(0) + 5..];
        rest = match body.find(END) {
            Some(end) => {
                examined(end + END.len());
                &body[end + END.len()..]
            }
            None => {
                examined(body.len());
                ""
            }
        };
    }
    // The search that found no further marker looked at all of what was left.
    examined(rest.len());
    out.push_str(rest);
    out
}

impl Redactor {
    /// A redactor for these environment values. Values shorter than
    /// [`MIN_REDACTED_VALUE_CHARS`] are not used.
    pub fn new<I: IntoIterator<Item = String>>(values: I) -> Self {
        let mut values: Vec<String> = values
            .into_iter()
            .filter(|v| v.chars().count() >= MIN_REDACTED_VALUE_CHARS)
            .collect();
        values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        values.dedup();
        Self { values }
    }

    /// The text with every known value and credential-shaped string removed.
    pub fn apply(&self, text: &str) -> String {
        let mut text = text.to_string();
        for value in &self.values {
            text = text.replace(value.as_str(), MASK);
        }
        let text = without_private_keys(&text);
        let mut out = String::with_capacity(text.len());
        let mut previous_word = String::new();
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let space_len = rest.len() - rest.trim_start().len();
            let (space, tail) = rest.split_at(space_len);
            out.push_str(space);
            let word_len = tail.find(char::is_whitespace).unwrap_or(tail.len());
            let (word, next) = tail.split_at(word_len);
            if !word.is_empty() {
                let after_bearer = previous_word.eq_ignore_ascii_case("bearer");
                if after_bearer && word.trim_matches(['"', '\'', ',']).len() >= 12 {
                    out.push_str(MASK);
                } else {
                    redact_word(word, &mut out);
                }
                previous_word = word.to_string();
            }
            rest = next;
        }
        out
    }
}

/// Copy `word`, replacing each credential-shaped token inside it.
fn redact_word(word: &str, out: &mut String) {
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if token.is_empty() {
            return;
        }
        out.push_str(if credential_shaped(token) {
            MASK
        } else {
            token
        });
        token.clear();
    };
    for c in word.chars() {
        if token_char(c) {
            token.push(c);
        } else {
            flush(&mut token, out);
            out.push(c);
        }
    }
    flush(&mut token, out);
}

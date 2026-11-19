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

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_-.~+/=".contains(c)
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

/// Replace private key blocks, up to their end line or the end of the text.
fn without_private_keys(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN ") {
        let after = &rest[start..];
        let header_end = after
            .find("-----\n")
            .or_else(|| after[11..].find("-----").map(|i| i + 11));
        let is_key = header_end.is_some_and(|i| after[..i].ends_with("PRIVATE KEY"));
        if !is_key {
            out.push_str(&rest[..start + 11]);
            rest = &rest[start + 11..];
            continue;
        }
        out.push_str(&rest[..start]);
        out.push_str(MASK);
        match after.find("PRIVATE KEY-----").and_then(|i| {
            after[i + 16..]
                .find("PRIVATE KEY-----")
                .map(|j| i + 16 + j + 16)
        }) {
            Some(end) => rest = &after[end..],
            None => {
                rest = "";
            }
        }
    }
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
        let mut token = String::new();
        let mut previous_was_bearer = false;
        let flush = |token: &mut String, out: &mut String, bearer: &mut bool| {
            if token.is_empty() {
                return;
            }
            let hide = credential_shaped(token) || (*bearer && token.len() >= 12);
            *bearer = token.eq_ignore_ascii_case("bearer");
            out.push_str(if hide { MASK } else { token });
            token.clear();
        };
        for c in text.chars() {
            if token_char(c) {
                token.push(c);
            } else {
                flush(&mut token, &mut out, &mut previous_was_bearer);
                if !c.is_whitespace() {
                    previous_was_bearer = false;
                }
                out.push(c);
            }
        }
        flush(&mut token, &mut out, &mut previous_was_bearer);
        out
    }
}

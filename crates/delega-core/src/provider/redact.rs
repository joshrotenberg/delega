//! Removing secrets from provider text before it goes into a result.
//!
//! Provider standard error and error messages can echo what the provider was
//! given. Two things are removed: the values of the environment variables the
//! provider was started with when the variable is secret-looking or belongs to
//! the provider, and strings with the shape of a common credential (API keys,
//! cloud and forge tokens, JSON web tokens, bearer tokens, private key blocks).
//! The shapes are a heuristic, so a credential in an unusual form is not caught.

use super::limits::MIN_REDACTED_VALUE_CHARS;

/// What a removed secret is replaced with.
pub(super) const MASK: &str = "[redacted]";

/// What to remove.
#[derive(Clone, Default)]
pub(super) struct Redactor {
    values: Vec<Known>,
}

/// One known value and its prefix table (see [`prefix_table`]).
#[derive(Clone)]
struct Known {
    text: String,
    table: Vec<usize>,
}

/// For each prefix of `pattern`, the length of its longest proper prefix that
/// is also its suffix (the Knuth-Morris-Pratt table), so a scan for the
/// pattern never goes back in the text.
fn prefix_table(pattern: &[u8]) -> Vec<usize> {
    let mut table = vec![0; pattern.len()];
    let mut k = 0;
    for i in 1..pattern.len() {
        while k > 0 && pattern[k] != pattern[i] {
            k = table[k - 1];
        }
        if pattern[k] == pattern[i] {
            k += 1;
        }
        table[i] = k;
    }
    table
}

/// Add the byte span of every place `pattern` occurs in `text` to `spans`,
/// overlapping occurrences included, in one pass over `text`.
fn occurrences(text: &[u8], pattern: &[u8], table: &[usize], spans: &mut Vec<(usize, usize)>) {
    if pattern.is_empty() {
        return;
    }
    let mut k = 0;
    for (i, &byte) in text.iter().enumerate() {
        while k > 0 && pattern[k] != byte {
            k = table[k - 1];
        }
        if pattern[k] == byte {
            k += 1;
        }
        if k == pattern.len() {
            spans.push((i + 1 - k, i + 1));
            k = table[k - 1];
        }
    }
}

/// The length of the longest end of `value` that `start` begins with, in one
/// pass over `value`. `start` is no longer than `value` minus one byte, so the
/// end is a proper one.
fn longest_end_at_start(value: &[u8], start: &[u8]) -> usize {
    if start.is_empty() {
        return 0;
    }
    let table = prefix_table(start);
    let mut k = 0;
    for &byte in value {
        if k == start.len() {
            k = table[k - 1];
        }
        while k > 0 && start[k] != byte {
            k = table[k - 1];
        }
        if start[k] == byte {
            k += 1;
        }
    }
    k
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
        let values = values
            .into_iter()
            .map(|text| Known {
                table: prefix_table(text.as_bytes()),
                text,
            })
            .collect();
        Self { values }
    }

    /// The text with every known value and credential-shaped string removed.
    pub fn apply(&self, text: &str) -> String {
        self.redact(text, None)
    }

    /// [`Self::apply`] for the decoded text of a capture whose beginning was
    /// cut off. The capture can begin with the end of a known value, which
    /// has no beginning left to be found by. Such an end (a proper suffix at
    /// least [`MIN_REDACTED_VALUE_CHARS`] long) is removed with the whole
    /// values, in the same pass.
    ///
    /// The cut can also split a character. Its remaining bytes, at most
    /// three, are UTF-8 continuation bytes that decode to one replacement
    /// character each. Those, and no other replacement characters, are
    /// stepped over before the end of a value is looked for, and go with it.
    pub fn apply_cut(&self, capture: &[u8]) -> String {
        let stray = capture
            .iter()
            .take(3)
            .take_while(|byte| (0x80..=0xBF).contains(*byte))
            .count();
        let cut_chars: usize = String::from_utf8_lossy(&capture[..stray])
            .chars()
            .take_while(|c| *c == char::REPLACEMENT_CHARACTER)
            .map(char::len_utf8)
            .sum();
        self.redact(&String::from_utf8_lossy(capture), Some(cut_chars))
    }

    /// The byte spans of `text` that hold known values, merged and in order.
    /// Every occurrence of every value counts, overlapping ones included, and
    /// for a cut capture (`cut_chars` is the length of the replacement
    /// characters its cut left) the end of a value at the start. Spans that
    /// overlap or touch are one span, so a value that overlaps another is
    /// removed whole, whichever is found first.
    fn value_spans(&self, text: &str, cut_chars: Option<usize>) -> Vec<(usize, usize)> {
        let mut spans = Vec::new();
        for value in &self.values {
            // The table scan is linear but slower than `contains`, and most
            // text holds no value.
            if text.contains(value.text.as_str()) {
                occurrences(
                    text.as_bytes(),
                    value.text.as_bytes(),
                    &value.table,
                    &mut spans,
                );
            }
            let Some(body) = cut_chars.and_then(|skip| text.get(skip..)) else {
                continue;
            };
            let longest = body.len().min(value.text.len() - 1);
            let len = longest_end_at_start(value.text.as_bytes(), &body.as_bytes()[..longest]);
            // Both texts are whole characters, so equal bytes that end where
            // the value ends are whole characters too.
            let enough = body
                .get(..len)
                .is_some_and(|end| end.chars().count() >= MIN_REDACTED_VALUE_CHARS);
            if enough {
                spans.push((0, cut_chars.unwrap_or(0) + len));
            }
        }
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
        for (start, end) in spans {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        merged
    }

    /// Known values are replaced in one pass over the text as it was given:
    /// each merged span of [`Self::value_spans`] becomes one [`MASK`], and a
    /// mask is never searched again. Every span is at least
    /// [`MIN_REDACTED_VALUE_CHARS`] characters, so this pass makes text longer
    /// by at most `MASK.len() / MIN_REDACTED_VALUE_CHARS`. The passes after it
    /// (private keys, credential shapes, `Bearer`) only make text shorter.
    fn redact(&self, text: &str, cut_chars: Option<usize>) -> String {
        let spans = self.value_spans(text, cut_chars);
        let mut without_values = String::with_capacity(text.len());
        let mut copied = 0;
        for (start, end) in spans {
            without_values.push_str(&text[copied..start]);
            without_values.push_str(MASK);
            copied = end;
        }
        without_values.push_str(&text[copied..]);
        let text = without_values;
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

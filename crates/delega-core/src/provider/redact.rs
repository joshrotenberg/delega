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

/// Report the byte span of every place `pattern` occurs in `text` to `found`,
/// overlapping occurrences included, in one pass over `text`.
fn occurrences(text: &[u8], pattern: &[u8], table: &[usize], mut found: impl FnMut(usize, usize)) {
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
            found(i + 1 - k, i + 1);
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

/// Report the byte span of every private key block in `text` to `found`: from
/// its begin marker up to its end line, or to the end of the text when it has
/// none. One forward pass: every search starts where the last one stopped, and
/// a begin marker that is not a key is skipped after a bounded look.
fn private_key_blocks(text: &str, mut found: impl FnMut(usize, usize)) {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "PRIVATE KEY-----";
    let mut from = 0;
    while let Some(at) = text[from..].find(BEGIN) {
        examined(at + BEGIN.len());
        let start = from + at;
        let label = start + BEGIN.len();
        let after = &text.as_bytes()[label..];
        let window = &after[..after.len().min(BEGIN_LABEL_WINDOW)];
        examined(window.len());
        let label_end = window.windows(5).position(|w| w == b"-----");
        let Some(label_end) = label_end.filter(|i| after[..*i].ends_with(b"PRIVATE KEY")) else {
            from = label;
            continue;
        };
        let body = label + label_end + 5;
        from = match text[body..].find(END) {
            Some(end) => {
                examined(end + END.len());
                body + end + END.len()
            }
            None => {
                examined(text.len() - body);
                text.len()
            }
        };
        found(start, from);
    }
    // The search that found no further marker looked at all of what was left.
    examined(text.len() - from);
}

/// Replace private key blocks (see [`private_key_blocks`]).
fn without_private_keys(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    private_key_blocks(text, |start, end| {
        out.push_str(&text[copied..start]);
        out.push_str(MASK);
        copied = end;
    });
    out.push_str(&text[copied..]);
    out
}

/// Which bytes of a text are to be replaced, as a difference array: one more
/// at the start of each marked span and one less at its end, so the running
/// sum is above zero exactly on the marked bytes. It takes one entry for each
/// byte of the text and one more, however many spans are marked, and nothing
/// until the first span.
struct Coverage {
    len: usize,
    steps: Vec<i32>,
}

impl Coverage {
    fn new(len: usize) -> Self {
        Self {
            len,
            steps: Vec::new(),
        }
    }

    /// Mark the bytes `start..end`. Any one start or end is marked at most
    /// once for each known value, private key block and cut start, so an
    /// entry stays far inside its range.
    fn mark(&mut self, start: usize, end: usize) {
        if start >= end || end > self.len {
            return;
        }
        if self.steps.is_empty() {
            self.steps = vec![0; self.len + 1];
        }
        self.steps[start] += 1;
        self.steps[end] -= 1;
    }

    /// `text` with each run of marked bytes replaced by one [`MASK`]. Marked
    /// spans that overlap or touch are one run.
    fn masked(&self, text: &str) -> String {
        if self.steps.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut depth = 0i64;
        let mut copied = Some(0);
        for (at, step) in self.steps[..self.len].iter().enumerate() {
            depth += i64::from(*step);
            match copied {
                Some(from) if depth > 0 => {
                    out.push_str(&text[from..at]);
                    out.push_str(MASK);
                    copied = None;
                }
                None if depth <= 0 => copied = Some(at),
                _ => {}
            }
        }
        if let Some(from) = copied {
            out.push_str(&text[from..]);
        }
        out
    }
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

    /// Which bytes of `text` are secrets that are found in the text as it was
    /// given: every occurrence of every known value, overlapping ones
    /// included; for a cut capture (`cut_chars` is the length of the
    /// replacement characters its cut left) the end of a value at the start;
    /// and every private key block. Each span starts and ends between
    /// characters: a value and the text are both whole characters, a block
    /// starts and ends at its markers or the end of the text, and the cut
    /// start is checked below.
    fn coverage(&self, text: &str, cut_chars: Option<usize>) -> Coverage {
        let mut covered = Coverage::new(text.len());
        for value in &self.values {
            // The table scan is linear but slower than `contains`, and most
            // text holds no value.
            if text.contains(value.text.as_str()) {
                occurrences(
                    text.as_bytes(),
                    value.text.as_bytes(),
                    &value.table,
                    |start, end| covered.mark(start, end),
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
                covered.mark(0, cut_chars.unwrap_or(0) + len);
            }
        }
        // On the text as it was given, so a value that overlaps a block's
        // begin marker cannot hide the block from the pass below.
        private_key_blocks(text, |start, end| covered.mark(start, end));
        covered
    }

    /// Known values and private key blocks are replaced in one pass over the
    /// text as it was given: each run of [`Self::coverage`] becomes one
    /// [`MASK`], and a mask is never searched for values again. Every run is
    /// at least [`MIN_REDACTED_VALUE_CHARS`] characters, so this pass makes
    /// text longer by at most `MASK.len() / MIN_REDACTED_VALUE_CHARS`. The
    /// passes after it (private keys again, credential shapes, `Bearer`) only
    /// make text shorter.
    fn redact(&self, text: &str, cut_chars: Option<usize>) -> String {
        let text = self.coverage(text, cut_chars).masked(text);
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

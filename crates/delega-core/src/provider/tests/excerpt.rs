//! The standard error excerpt and the guard at the start of a cut capture.

use crate::process::Captured;
use crate::provider::limits::{
    MAX_STDERR_BYTES, MIN_REDACTED_VALUE_CHARS, STDERR_CUT_START_BYTES, STDERR_EXCERPT_CHARS,
};
use crate::provider::redact::{MASK, Redactor};
use crate::provider::settle::{STDERR_CUT_GUARD_BYTES, excerpt};

/// Twenty long keys, which redact to twenty markers: less than one excerpt.
fn keys() -> String {
    format!("sk-ant-{}\n", "a".repeat(2000)).repeat(20)
}

fn masks() -> String {
    vec![MASK; 20].join(" ")
}

fn capture(text: &[u8], truncated: bool) -> Captured {
    Captured {
        bytes: text.to_vec(),
        total_bytes: text.len() as u64 + if truncated { 1000 } else { 0 },
        truncated,
    }
}

fn none() -> Redactor {
    Redactor::default()
}

#[test]
fn the_guard_covers_the_redacted_image_of_the_start_of_a_cut_capture() {
    assert_eq!(STDERR_CUT_START_BYTES, 8 * 1024);
    assert_eq!(STDERR_CUT_GUARD_BYTES, 13_664);
    // The text that redaction lengthens most: the shortest known value over
    // and over, the last one lying across the end of the guarded start.
    let value = "v".repeat(MIN_REDACTED_VALUE_CHARS);
    let count = STDERR_CUT_START_BYTES.div_ceil(value.len());
    let redactor = Redactor::new([value.clone()]);
    let image = redactor.apply(&value.repeat(count));
    assert_eq!(image.len(), count * MASK.len());
    assert!(image.len() <= STDERR_CUT_GUARD_BYTES, "{}", image.len());
    // And end to end: that start, then text to show.
    let text = format!("{}\n{}", value.repeat(count), "shown ".repeat(3000));
    assert!(text.len() <= MAX_STDERR_BYTES);
    let out = excerpt(&capture(text.as_bytes(), true), &redactor).expect("excerpt");
    assert!(!out.contains(MASK), "{out}");
    assert!(out.ends_with("shown shown"), "{out}");
}

#[test]
fn a_cut_capture_shows_nothing_from_before_the_guard() {
    for fragment in [
        "mnopqrstuvwxyz0123456789\n".to_string(),
        "abcdefghijklmnop123\n".to_string(),
        "cutbodyline0123456789\n-----END RSA PRIVATE KEY-----\n".to_string(),
        // A fragment as long as the guarded start, in distinct pieces.
        (0..1170).map(|i| format!("p{i:04}z\n")).collect::<String>(),
    ] {
        let text = format!("{fragment}{}", keys());
        let shown = excerpt(&capture(text.as_bytes(), false), &none()).expect("not cut");
        let last = fragment.split_whitespace().last().expect("a word");
        assert!(shown.contains(last), "not cut, the start shows: {shown}");
        assert_eq!(excerpt(&capture(text.as_bytes(), true), &none()), None);
    }
}

#[test]
fn a_cut_capture_shows_only_text_after_the_guard() {
    // The fragment, lines up to the guard, then lines after it.
    let fragment = "mnopqrstuvwxyz0123456789\n";
    let lines = (STDERR_CUT_GUARD_BYTES - fragment.len()) / 7;
    let pad = "\n".repeat(STDERR_CUT_GUARD_BYTES - fragment.len() - lines * 7);
    let before = format!("{}{pad}", "before\n".repeat(lines));
    let text = format!("{fragment}{before}after one\nafter two\n");
    assert_eq!(fragment.len() + before.len(), STDERR_CUT_GUARD_BYTES);
    let out = excerpt(&capture(text.as_bytes(), true), &none()).expect("excerpt");
    assert!(!out.contains("before"), "{out}");
    assert!(out.ends_with("after one after two"), "{out}");
    assert!(out.len() < 40, "{out}");
}

#[test]
fn a_cut_capture_with_only_whitespace_after_the_guard_has_no_excerpt() {
    let text = format!("fragment{}", "\n".repeat(20_000));
    assert_eq!(excerpt(&capture(text.as_bytes(), true), &none()), None);
    // Not cut, the same start is shown when it is within the last 8 KiB.
    let text = format!("fragment{}", "\n".repeat(1000));
    assert_eq!(
        excerpt(&capture(text.as_bytes(), false), &none()).as_deref(),
        Some("fragment")
    );
    assert_eq!(excerpt(&capture(text.as_bytes(), true), &none()), None);
    assert_eq!(excerpt(&capture(b"", true), &none()), None);
}

#[test]
fn the_guard_moves_forward_to_a_character_boundary() {
    // Three-byte characters: the guard, not a multiple of three, falls
    // inside one.
    assert_ne!(STDERR_CUT_GUARD_BYTES % 3, 0);
    let text = "\u{20AC}".repeat(6000);
    let out = excerpt(&capture(text.as_bytes(), true), &none()).expect("excerpt");
    assert_eq!(
        out,
        format!("... {}", "\u{20AC}".repeat(STDERR_EXCERPT_CHARS))
    );
}

#[test]
fn a_character_cut_by_the_start_of_a_capture_is_not_shown() {
    // The second byte of a two-byte character, as the capture's first byte.
    let mut bytes = vec![0xA9, b'\n'];
    bytes.extend_from_slice(keys().as_bytes());
    let shown = excerpt(&capture(&bytes, false), &none()).expect("not cut");
    assert_eq!(shown, format!("\u{FFFD} {}", masks()));
    assert_eq!(excerpt(&capture(&bytes, true), &none()), None);
}

#[test]
fn a_capture_that_was_not_cut_keeps_its_excerpt() {
    let redactor = Redactor::new(["known-value-0123456789".to_string()]);
    // The end of a known value at the start is left alone when nothing was
    // cut: it is ordinary text there.
    let text = format!("0123456789 first words\n{}", keys());
    assert_eq!(
        excerpt(&capture(text.as_bytes(), false), &redactor),
        Some(format!("0123456789 first words {}", masks()))
    );
    let long = format!("start {} end", "b".repeat(400));
    assert_eq!(
        excerpt(&capture(long.as_bytes(), false), &redactor),
        Some(format!("... {} end", "b".repeat(STDERR_EXCERPT_CHARS - 4)))
    );
}

#[test]
fn the_end_of_a_known_value_longer_than_the_guard_is_masked_at_a_cut_start() {
    let value: String = (0..3000).map(|i| format!("v{i:05}q")).collect();
    let redactor = Redactor::new([value.clone()]);
    // The capture starts inside the value and keeps more of it than the
    // guard removes.
    let kept = &value[value.len() - 16_000..];
    assert!(kept.len() > STDERR_CUT_GUARD_BYTES);
    let text = format!("{kept}\n{}", keys());
    assert_eq!(excerpt(&capture(text.as_bytes(), true), &redactor), None);
    // With text after the guard, the excerpt has none of the value.
    let text = format!("{kept}\n{}", "shown ".repeat(4000));
    let out = excerpt(&capture(text.as_bytes(), true), &redactor).expect("excerpt");
    assert!(!out.contains('q'), "{out}");
    assert!(out.ends_with("shown shown"), "{out}");
}

#[test]
fn mask_cut_start_replaces_the_longest_known_value_end_and_nothing_else() {
    let redactor = Redactor::new([
        "alpha-secret-tail".to_string(),
        "other-long-secret-tail".to_string(),
        "caf\u{e9}-secret-\u{e9}t\u{e9}-value".to_string(),
    ]);
    // "secret-tail" ends both values; the longer end wins.
    assert_eq!(
        redactor.mask_cut_start("long-secret-tail then text"),
        "[redacted] then text"
    );
    assert_eq!(
        redactor.mask_cut_start("secret-tail then text"),
        "[redacted] then text"
    );
    // An end shorter than the shortest redacted value is left.
    assert_eq!(MIN_REDACTED_VALUE_CHARS, 6);
    assert_eq!(redactor.mask_cut_start("t-tail then"), "[redacted] then");
    assert_eq!(redactor.mask_cut_start("-tail then"), "-tail then");
    // A whole value is not an end of itself; `apply` replaces it.
    let whole = "alpha-secret-tail then";
    assert_eq!(redactor.mask_cut_start(whole), whole);
    assert_eq!(redactor.apply(whole), "[redacted] then");
    // Only at the very start.
    let later = "x secret-tail";
    assert_eq!(redactor.mask_cut_start(later), later);
    // A value cut inside a character: the replacement characters left by the
    // cut go with it. Characters are counted, not bytes.
    assert_eq!(
        redactor.mask_cut_start("\u{FFFD}-secret-\u{e9}t\u{e9}-value rest"),
        "[redacted] rest"
    );
    assert_eq!(
        redactor.mask_cut_start("\u{e9}-value rest"),
        "[redacted] rest"
    );
    assert_eq!(redactor.mask_cut_start("-value rest"), "[redacted] rest");
    assert_eq!(redactor.mask_cut_start("value rest"), "value rest");
    assert_eq!(none().mask_cut_start("anything"), "anything");
}

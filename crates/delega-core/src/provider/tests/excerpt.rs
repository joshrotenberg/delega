//! The standard error excerpt and the guard at the start of a cut capture.

use crate::process::Captured;
use crate::provider::limits::{
    MIN_REDACTED_VALUE_CHARS, STDERR_CUT_START_BYTES, STDERR_EXCERPT_CHARS,
};
use crate::provider::redact::{MASK, Redactor};
use crate::provider::settle::{STDERR_CUT_GUARD_BYTES, STDERR_NOT_SHOWN, excerpt, stderr_note};

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

/// A cut capture whose first [`STDERR_CUT_START_BYTES`] are `unit` over and
/// over and, as their last two bytes, the sentinel `ZQ`; then the keys, which
/// redact to less than one excerpt. Without the guard the sentinel is in the
/// excerpt.
fn sentinel_at_the_end_of_the_guarded_start(unit: &str) -> Captured {
    let count = (STDERR_CUT_START_BYTES - 2) / unit.len();
    let pad = ".".repeat(STDERR_CUT_START_BYTES - 2 - count * unit.len());
    let text = format!("{pad}{}ZQ\n{}", unit.repeat(count), keys());
    assert_eq!(text.find("ZQ"), Some(STDERR_CUT_START_BYTES - 2));
    capture(text.as_bytes(), true)
}

#[test]
fn the_guard_covers_the_redacted_image_of_the_start_of_a_cut_capture() {
    assert_eq!(STDERR_CUT_START_BYTES, 8 * 1024);
    assert_eq!(STDERR_CUT_GUARD_BYTES, 13_664);
    // The text that redaction lengthens most: the shortest known value over
    // and over. Occurrences that touch are one span, so a space keeps them
    // apart and each becomes its own marker.
    let value = "v".repeat(MIN_REDACTED_VALUE_CHARS);
    let redactor = Redactor::new([value.clone()]);
    let unit = format!("{value} ");
    assert_eq!(
        redactor.apply(&unit.repeat(100)).len(),
        100 * (MASK.len() + 1)
    );
    let cut = sentinel_at_the_end_of_the_guarded_start(&unit);
    // The start grew, and the sentinel after it is still before the guard.
    let image = redactor.apply_cut(&cut.bytes);
    let at = image.find("ZQ").expect("the sentinel is not a secret");
    assert!(at > STDERR_CUT_START_BYTES * 3 / 2, "{at}");
    assert!(at + 2 <= STDERR_CUT_GUARD_BYTES, "{at}");
    assert_eq!(excerpt(&cut, &redactor), None);
    // Not cut, the same text shows it: the tail redacts short enough.
    let shown = excerpt(&capture(&cut.bytes, false), &redactor).expect("not cut");
    assert!(shown.contains(" ZQ [redacted]"), "{shown}");
}

#[test]
fn a_value_that_matches_inside_a_marker_does_not_lengthen_the_text_past_the_guard() {
    // "redact" is in "[redacted]". Replaced one value after another, each
    // seven bytes became fourteen and the sentinel moved past the guard.
    let redactor = Redactor::new(["abcdefg".to_string(), "redact".to_string()]);
    assert_eq!(redactor.apply("abcdefg"), MASK);
    assert_eq!(
        redactor.apply("x abcdefg y redact z"),
        "x [redacted] y [redacted] z"
    );
    for unit in ["abcdefg", "abcdefg "] {
        let cut = sentinel_at_the_end_of_the_guarded_start(unit);
        let out = excerpt(&cut, &redactor);
        assert!(!out.clone().unwrap_or_default().contains("ZQ"), "{out:?}");
    }
}

#[test]
fn a_cut_start_that_ends_one_value_and_begins_another_hides_the_whole_other_value() {
    // The capture starts with all of `long`, whose first six characters also
    // end `other`. Masking those six first left the rest of `long` unmatched.
    let long: String = format!(
        "shared{}",
        (0..2500).map(|i| format!("w{i:05}k")).collect::<String>()
    );
    assert!(long.len() > STDERR_CUT_GUARD_BYTES);
    let redactor = Redactor::new(["another-value-shared".to_string(), long.clone()]);
    let text = format!("{long}\n{}", keys());
    let out = excerpt(&capture(text.as_bytes(), true), &redactor);
    assert!(!out.clone().unwrap_or_default().contains('k'), "{out:?}");
    assert_eq!(out, None);
    assert_eq!(
        redactor.apply_cut(format!("{long} rest").as_bytes()),
        "[redacted] rest"
    );
}

#[test]
fn replacement_characters_that_are_part_of_a_value_are_not_stepped_over_at_a_cut_start() {
    // The value ends in 6000 literal U+FFFD, and the capture keeps 5500 of
    // them, more than the guard removes. Only replacement characters the cut
    // itself made may be stepped over; these are the value.
    let value = format!("known-{}", "\u{FFFD}".repeat(6000));
    let redactor = Redactor::new([value]);
    let kept = "\u{FFFD}".repeat(5500);
    assert!(kept.len() > STDERR_CUT_GUARD_BYTES);
    let text = format!("{kept}\n{}", keys());
    let out = excerpt(&capture(text.as_bytes(), true), &redactor);
    assert!(
        !out.clone().unwrap_or_default().contains('\u{FFFD}'),
        "{out:?}"
    );
    assert_eq!(out, None);
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
fn known_values_are_replaced_in_one_pass_with_overlaps_merged() {
    let redactor = Redactor::new([
        "abab-abab".to_string(),
        "secret-one".to_string(),
        "one-and-two".to_string(),
        "redact".to_string(),
    ]);
    // An occurrence that overlaps the one before it is found too.
    assert_eq!(redactor.apply("x abab-abab-abab y"), "x [redacted] y");
    // Two values that overlap, and two that touch, are one marker.
    assert_eq!(redactor.apply("a secret-one-and-two b"), "a [redacted] b");
    assert_eq!(
        redactor.apply("a secret-oneone-and-two b"),
        "a [redacted] b"
    );
    // A marker is not searched for values.
    assert_eq!(
        redactor.apply("secret-one, redact"),
        "[redacted], [redacted]"
    );
    let plain = "nothing of the kind: abab-aba, secret-on";
    assert_eq!(redactor.apply(plain), plain);
}

#[test]
fn apply_cut_replaces_the_longest_known_value_end_and_nothing_else() {
    let redactor = Redactor::new([
        "alpha-secret-tail".to_string(),
        "other-long-secret-tail".to_string(),
        "caf\u{e9}-secret-\u{e9}t\u{e9}-value".to_string(),
    ]);
    let cut = |text: &str| redactor.apply_cut(text.as_bytes());
    // "secret-tail" ends both values; the longer end wins.
    assert_eq!(cut("long-secret-tail then text"), "[redacted] then text");
    assert_eq!(cut("secret-tail then text"), "[redacted] then text");
    // An end shorter than the shortest redacted value is left.
    assert_eq!(MIN_REDACTED_VALUE_CHARS, 6);
    assert_eq!(cut("t-tail then"), "[redacted] then");
    assert_eq!(cut("-tail then"), "-tail then");
    // A whole value, at the start and later, as `apply` does it.
    assert_eq!(cut("alpha-secret-tail then"), "[redacted] then");
    assert_eq!(
        cut("t-tail, alpha-secret-tail then"),
        "[redacted], [redacted] then"
    );
    // The end of a value only counts at the very start.
    assert_eq!(cut("x secret-tail"), "x secret-tail");
    assert_eq!(redactor.apply("secret-tail then"), "secret-tail then");
    // Characters are counted, not bytes.
    assert_eq!(cut("\u{e9}-value rest"), "[redacted] rest");
    assert_eq!(cut("-value rest"), "[redacted] rest");
    assert_eq!(cut("value rest"), "value rest");
    assert_eq!(none().apply_cut(b"anything"), "anything");
    assert_eq!(none().apply_cut(b""), "");
}

#[test]
fn apply_cut_steps_over_only_the_bytes_of_a_character_the_cut_split() {
    let redactor = Redactor::new([
        "caf\u{e9}-secret-value".to_string(),
        "\u{1F600}-emoji-value".to_string(),
        "x\u{FFFD}\u{FFFD}-literal-value".to_string(),
    ]);
    // The second byte of the two-byte character, then the rest of the value.
    assert_eq!(
        redactor.apply_cut(b"\xA9-secret-value rest"),
        "[redacted] rest"
    );
    // The last three bytes of a four-byte character.
    assert_eq!(
        redactor.apply_cut(b"\x9F\x98\x80-emoji-value rest"),
        "[redacted] rest"
    );
    // Four stray bytes are not one cut character: three are stepped over and
    // the fourth is in the way.
    assert_eq!(
        redactor.apply_cut(b"\x80\x80\x80\x80-emoji-value rest"),
        "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}-emoji-value rest"
    );
    // Replacement characters that were written as such are text: they match
    // a value that holds them, and are not stepped over for one that does not.
    assert_eq!(
        redactor.apply_cut("\u{FFFD}\u{FFFD}-literal-value rest".as_bytes()),
        "[redacted] rest"
    );
    assert_eq!(
        redactor.apply_cut("\u{FFFD}-secret-value rest".as_bytes()),
        "\u{FFFD}-secret-value rest"
    );
    // A cut character with no value after it stays as it decoded.
    assert_eq!(redactor.apply_cut(b"\xA9 plain"), "\u{FFFD} plain");
}

#[test]
fn a_value_that_overlaps_a_private_key_begin_marker_does_not_hide_the_key() {
    // The two values overlap, and together they cover the block's begin
    // marker. Replaced before the block was looked for, they left a body
    // with no marker in front of it.
    let redactor = Redactor::new([
        "a-long-prefix-OVERLAP".to_string(),
        "OVERLAP-----BEGIN".to_string(),
    ]);
    let block = "a-long-prefix-OVERLAP-----BEGIN PRIVATE KEY-----\nMIIEkeybody0123456789\n\
                 -----END PRIVATE KEY-----";
    let out = redactor.apply(&format!("before {block} after"));
    assert!(!out.contains("MIIEkeybody"), "{out}");
    assert_eq!(out, "before [redacted] after");
    // A block with no end line goes to the end of the text, as before.
    let open = "a-long-prefix-OVERLAP-----BEGIN RSA PRIVATE KEY-----\nMIIEkeybody0123456789\nmore";
    assert_eq!(
        redactor.apply(&format!("before {open}")),
        "before [redacted]"
    );
    // A value across the end line: the block is still one marker.
    let redactor = Redactor::new(["KEY-----tail-of-value".to_string()]);
    let text =
        "x -----BEGIN PRIVATE KEY-----\nMIIEkeybody\n-----END PRIVATE KEY-----tail-of-value y";
    assert_eq!(redactor.apply(text), "x [redacted] y");
    // A begin marker that is not a key is only text, with or without a value
    // on it.
    let redactor = Redactor::new(["OVERLAP-----BEGIN".to_string()]);
    assert_eq!(
        redactor.apply("x OVERLAP-----BEGIN CERTIFICATE-----\nMIIB\n y"),
        "x [redacted] CERTIFICATE-----\nMIIB\n y"
    );
    // The same in a cut capture, through the excerpt.
    let redactor = Redactor::new([
        "a-long-prefix-OVERLAP".to_string(),
        "OVERLAP-----BEGIN".to_string(),
    ]);
    let text = format!("{}{block}\nend\n", "plain line\n".repeat(1500));
    let out = excerpt(&capture(text.as_bytes(), true), &redactor).expect("excerpt");
    assert!(!out.contains("MIIEkeybody"), "{out}");
    assert!(out.ends_with("plain line [redacted] end"), "{out}");
}

#[test]
fn many_repetitive_values_on_a_repetitive_capture_are_one_marker() {
    // Every one of these 500 values occurs at nearly every byte of the
    // capture: about 32 million occurrences. They are marked on the text, not
    // collected.
    let redactor = Redactor::new((6..=505).map(|len| "a".repeat(len)));
    let all = "a".repeat(64 * 1024);
    // Five do not make a value, so the runs at the ends are kept.
    let text = format!("aaaaa {all} aaaaa");
    assert_eq!(redactor.apply(&text), "aaaaa [redacted] aaaaa");
    assert_eq!(redactor.apply_cut(all.as_bytes()), MASK);
    // Runs that do not touch stay apart, and what is between them is kept.
    let runs = format!("{} ", "a".repeat(654)).repeat(100);
    assert_eq!(redactor.apply(&runs), "[redacted] ".repeat(100));
}

#[test]
fn a_cut_capture_with_no_excerpt_says_so_whatever_left_it_without_one() {
    let note = format!(" {STDERR_NOT_SHOWN}");
    // A word after the guard, and a blank last 8 KiB: the guard left text,
    // and still there is nothing to show.
    let mut blank_end = vec![b' '; 64 * 1024];
    blank_end[30_000..30_004].copy_from_slice(b"word");
    const { assert!(30_000 > STDERR_CUT_GUARD_BYTES) };
    assert_eq!(excerpt(&capture(&blank_end, true), &none()), None);
    assert_eq!(stderr_note(&capture(&blank_end, true), &none()), note);
    // Nothing after the guard.
    let short = format!("fragment\n{}", keys());
    assert_eq!(stderr_note(&capture(short.as_bytes(), true), &none()), note);
    // With an excerpt, the excerpt.
    let text = "shown ".repeat(4000);
    assert_eq!(
        stderr_note(&capture(text.as_bytes(), true), &none()),
        format!(
            "; its standard error ended with: ... {}",
            " shown".repeat(50)
        )
    );
    // A capture that was not cut says nothing when it has no text.
    assert_eq!(stderr_note(&capture(&blank_end, false), &none()), "");
    assert_eq!(stderr_note(&capture(b"", false), &none()), "");
    assert_eq!(stderr_note(&capture(b" \n ", false), &none()), "");
}

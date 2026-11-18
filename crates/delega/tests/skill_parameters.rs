//! A skill must not name something this build lacks (docs/api.md, Skills).
//!
//! What is checked, for every embedded skill:
//!
//! - Inline code spans and fenced blocks are found by the markdown rules for
//!   backtick runs. A span or fence that is never closed is a failure.
//! - A span or fenced line that starts with `delega ` is read as a command
//!   line. The subcommand path must exist, nested ones included. Every flag
//!   before a `--` must belong to that subcommand. A value that follows a flag
//!   with a fixed set of values must be one of them. Tokens after `--` are
//!   provider options and are not checked.
//! - A span that begins with a flag (`--timeout 20m`, `-C`) names a flag that
//!   exists on some subcommand.
//! - A dotted span whose first part is a top-level request or result field
//!   (`permissions.mode`) must exist in the schema, following `$ref`s. Dotted
//!   spans that start with anything else (`result.json`, `0.1.0`) are ignored.
//! - A bare snake_case span (`timeout_secs`) must be a schema property, an
//!   enum value, an MCP tool or argument, a subcommand or skill name, or be in
//!   `OTHER_WORDS`.
//! - Table cells in a column headed `Outcome` must be `Outcome` values, and so
//!   must a snake_case word written as "outcome `word`" unless it is a known
//!   field.
//! - Table cells in a column headed `Request field` start with a request field.
//!   Spans after "request fields ...:" are request fields.
//! - In the `delegate` skill, the first column of a table headed `Tool` holds
//!   MCP tool names.
//!
//! What is not checked: prose without backticks, whether a described behavior
//! is true, spans that mix several of these shapes (such as `detail: "full"`),
//! the words after a flag that takes free text, and anything in a fenced
//! command line after the first `|`, `;`, `&&` or `>`.

use std::collections::{BTreeMap, BTreeSet};

use delega_core::contract::outcome::Outcome;
use delega_core::contract::schema::{REQUEST_SCHEMA, RESULT_SCHEMA};
use serde_json::Value;

/// The MCP tools and the arguments docs/api.md gives them. This moves to the
/// MCP crate, and is read from there, when the server exists.
const MCP_TOOLS: [(&str, &[&str]); 7] = [
    ("start", &["profile"]),
    ("follow_up", &["worker", "prompt", "timeout_secs"]),
    ("wait", &["runs", "workers", "timeout_secs", "after"]),
    ("read", &["run", "worker", "detail", "after"]),
    ("list", &[]),
    ("interrupt", &["run", "worker"]),
    ("close", &["worker"]),
];

/// Bare words a skill may put in code that are not schema properties, enum
/// values, tools, arguments or command names.
const OTHER_WORDS: [&str; 4] = [
    // Worker states, from the States rule in docs/api.md.
    "queued", "running", "idle", "closed",
];

/// One command in the clap tree.
#[derive(Default)]
struct Cmd {
    /// Long flags, each with its possible values (empty when free-form).
    longs: BTreeMap<String, Vec<String>>,
    shorts: BTreeMap<char, Vec<String>>,
    subs: BTreeSet<String>,
}

struct Known {
    /// Commands by path from the root; the root is the empty path.
    cmds: BTreeMap<Vec<String>, Cmd>,
    outcomes: BTreeSet<String>,
    request: Value,
    result: Value,
    /// Every bare word a snake_case span may be.
    words: BTreeSet<String>,
}

fn walk(cmd: &clap::Command, path: Vec<String>, out: &mut BTreeMap<Vec<String>, Cmd>) {
    let mut info = Cmd::default();
    for arg in cmd.get_arguments() {
        let values: Vec<String> = arg
            .get_possible_values()
            .iter()
            .filter(|v| !v.is_hide_set())
            .map(|v| v.get_name().to_string())
            .collect();
        if let Some(long) = arg.get_long() {
            info.longs.insert(long.to_string(), values.clone());
        }
        if let Some(short) = arg.get_short() {
            info.shorts.insert(short, values);
        }
    }
    for sub in cmd.get_subcommands() {
        info.subs.insert(sub.get_name().to_string());
        let mut next = path.clone();
        next.push(sub.get_name().to_string());
        walk(sub, next, out);
    }
    out.insert(path, info);
}

fn schema_words(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                match key.as_str() {
                    "properties" => {
                        if let Value::Object(props) = inner {
                            out.extend(props.keys().cloned());
                        }
                    }
                    "const" => {
                        if let Some(word) = inner.as_str() {
                            out.insert(word.to_string());
                        }
                    }
                    "enum" => out.extend(
                        inner
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|v| v.as_str().map(str::to_string)),
                    ),
                    _ => {}
                }
                schema_words(inner, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| schema_words(item, out)),
        _ => {}
    }
}

fn known() -> Known {
    let mut root = delega::command();
    root.build(); // adds the generated help flags and the `help` subcommand
    let mut cmds = BTreeMap::new();
    walk(&root, Vec::new(), &mut cmds);
    let outcomes: BTreeSet<String> = Outcome::ALL
        .iter()
        .map(|o| {
            serde_json::to_value(o)
                .expect("serialize outcome")
                .as_str()
                .expect("outcome is a string")
                .to_string()
        })
        .collect();
    let request: Value = serde_json::from_str(REQUEST_SCHEMA).expect("request schema");
    let result: Value = serde_json::from_str(RESULT_SCHEMA).expect("result schema");
    let mut words: BTreeSet<String> = outcomes.clone();
    schema_words(&request, &mut words);
    schema_words(&result, &mut words);
    for (tool, args) in MCP_TOOLS {
        words.insert(tool.to_string());
        words.extend(args.iter().map(|a| a.to_string()));
    }
    words.extend(OTHER_WORDS.iter().map(|w| w.to_string()));
    for info in cmds.values() {
        words.extend(info.subs.iter().cloned());
        words.extend(info.longs.values().flatten().cloned());
    }
    words.extend(
        delega_core::skills::catalog()
            .iter()
            .map(|s| s.name().to_string()),
    );
    Known {
        cmds,
        outcomes,
        request,
        result,
        words,
    }
}

// ---- markdown ----

/// The inline code spans and fenced blocks (as lines) of a document, and a
/// problem for each span or fence that is never closed.
fn tokenize(text: &str) -> (Vec<String>, Vec<Vec<String>>, Vec<String>) {
    let mut spans = Vec::new();
    let mut fences = Vec::new();
    let mut problems = Vec::new();
    // First pass: lift out fenced blocks, leaving a blank line in their place.
    let mut prose = String::new();
    let mut open: Option<(String, Vec<String>)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let fence_char = trimmed
            .chars()
            .next()
            .filter(|c| (*c == '`' || *c == '~') && trimmed.starts_with(&c.to_string().repeat(3)));
        match (&mut open, fence_char) {
            (None, Some(c)) => {
                let run: String = trimmed.chars().take_while(|x| *x == c).collect();
                open = Some((run, Vec::new()));
                prose.push('\n');
            }
            (Some((run, lines)), Some(c))
                if trimmed.starts_with(run.as_str()) && trimmed.trim_matches(c).is_empty() =>
            {
                fences.push(std::mem::take(lines));
                open = None;
                prose.push('\n');
            }
            (Some((_, lines)), _) => lines.push(line.to_string()),
            (None, None) => {
                prose.push_str(line);
                prose.push('\n');
            }
        }
    }
    if open.is_some() {
        problems.push("a fenced code block is never closed".to_string());
    }
    // Second pass: spans, one paragraph at a time, so one stray backtick
    // cannot change how the rest of the file is read.
    for paragraph in prose.split("\n\n") {
        let chars: Vec<char> = paragraph.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] != '`' {
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && chars[i] == '`' {
                i += 1;
            }
            let run = i - start;
            let mut j = i;
            let mut close = None;
            while j < chars.len() {
                if chars[j] == '`' {
                    let k = j;
                    while j < chars.len() && chars[j] == '`' {
                        j += 1;
                    }
                    if j - k == run {
                        close = Some(k);
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            if let Some(k) = close {
                let body: String = chars[i..k].iter().collect();
                spans.push(body.split_whitespace().collect::<Vec<_>>().join(" "));
                i = k + run;
            } else {
                let context: String = chars[start..].iter().take(40).collect();
                problems.push(format!("unbalanced backtick run near: {context:?}"));
                // Only this run is skipped; the rest of the paragraph is
                // still read.
            }
        }
    }
    (spans, fences, problems)
}

fn cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// The cells in `column` of every table whose header cell there is `header`.
fn table_column(text: &str, header: &str, column: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.starts_with('|') || cells(line).get(column).map(String::as_str) != Some(header) {
            continue;
        }
        lines.next(); // the separator row
        while let Some(row) = lines.peek().filter(|l| l.starts_with('|')) {
            if let Some(cell) = cells(row).get(column) {
                out.push(cell.clone());
            }
            lines.next();
        }
    }
    out
}

fn spans_of(cell: &str) -> Vec<String> {
    tokenize(&format!("{cell}\n")).0
}

/// Spans in the list after "request fields ...:" in a paragraph.
fn request_field_lists(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split("\n\n") {
        let Some(at) = paragraph.find("request fields") else {
            continue;
        };
        let rest = &paragraph[at..];
        let Some(colon) = rest.find(':') else {
            continue;
        };
        if rest[..colon].contains('`') {
            continue;
        }
        let list = &rest[colon..];
        let end = list.find("`.").map_or(list.len(), |n| n + 1);
        out.extend(spans_of(&list[..end]));
    }
    out
}

/// Words written as `outcome `word``.
fn outcome_mentions(text: &str) -> Vec<String> {
    text.match_indices("outcome `")
        .filter_map(|(at, _)| {
            let rest = &text[at + "outcome `".len()..];
            rest.find('`').map(|end| rest[..end].to_string())
        })
        .collect()
}

// ---- schema ----

fn variants<'a>(root: &'a Value, schema: &'a Value) -> Vec<&'a Value> {
    let mut out = vec![schema];
    if let Some(target) = schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix('#'))
        .and_then(|pointer| root.pointer(pointer))
    {
        out.extend(variants(root, target));
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        for part in schema
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            out.extend(variants(root, part));
        }
    }
    out
}

fn has_property(root: &Value, path: &str) -> bool {
    let mut current = vec![root];
    for segment in path.split('.') {
        let mut next = Vec::new();
        for schema in current {
            for variant in variants(root, schema) {
                if let Some(property) = variant.get("properties").and_then(|p| p.get(segment)) {
                    next.push(property);
                }
            }
        }
        if next.is_empty() {
            return false;
        }
        current = next;
    }
    true
}

fn is_snake(word: &str) -> bool {
    word.split('_').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    }) && word.starts_with(|c: char| c.is_ascii_lowercase())
}

fn is_dotted_identifier(span: &str) -> bool {
    span.contains('.') && span.split('.').all(is_snake)
}

// ---- command lines ----

fn is_placeholder(token: &str) -> bool {
    token.starts_with('<')
        || token.starts_with('[')
        || token.starts_with('$')
        || (!token.is_empty()
            && token
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b == b'_' || b == b'.'))
}

/// A flag token: `--long`, `--long=value`, `-x`, or `-xvalue`. Returns whether
/// it is long, its name, and an inline value.
fn flag_parts(token: &str) -> Option<(bool, String, Option<String>)> {
    if let Some(rest) = token.strip_prefix("--") {
        if !rest.starts_with(|c: char| c.is_ascii_lowercase()) {
            return None;
        }
        return Some(match rest.split_once('=') {
            Some((name, value)) => (true, name.to_string(), Some(value.to_string())),
            None => (true, rest.to_string(), None),
        });
    }
    let rest = token.strip_prefix('-')?;
    let first = rest.chars().next().filter(char::is_ascii_alphabetic)?;
    let tail = &rest[1..];
    Some((
        false,
        first.to_string(),
        (!tail.is_empty()).then(|| tail.trim_start_matches('=').to_string()),
    ))
}

/// Check one command line that starts with `delega`.
fn check_command_line(line: &str, known: &Known, out: &mut Vec<String>) {
    let mut tokens = line.split_whitespace();
    tokens.next(); // delega
    let mut path: Vec<String> = Vec::new();
    let mut pending: Option<(String, Vec<String>)> = None;
    for token in tokens {
        if matches!(token, "|" | ";" | "&&" | ">" | ">>" | "<" | "--") {
            break; // after `--` come provider options
        }
        if let Some((flag, values)) = pending.take() {
            if !is_placeholder(token) && !values.iter().any(|v| v == token) {
                out.push(format!(
                    "`{line}`: {token} is not a value of {flag} ({})",
                    values.join(", ")
                ));
            }
            continue;
        }
        let cmd = &known.cmds[&path];
        if let Some((long, name, inline)) = flag_parts(token) {
            let values = if long {
                cmd.longs.get(&name)
            } else {
                name.chars().next().and_then(|c| cmd.shorts.get(&c))
            };
            let shown = if long {
                format!("--{name}")
            } else {
                format!("-{name}")
            };
            let Some(values) = values else {
                let at = if path.is_empty() {
                    "delega".to_string()
                } else {
                    format!("delega {}", path.join(" "))
                };
                out.push(format!("`{line}`: {shown} is not an option of `{at}`"));
                continue;
            };
            if !values.is_empty() {
                match inline {
                    Some(v) if is_placeholder(&v) || values.contains(&v) => {}
                    Some(v) => out.push(format!(
                        "`{line}`: {v} is not a value of {shown} ({})",
                        values.join(", ")
                    )),
                    None => pending = Some((shown, values.clone())),
                }
            }
            continue;
        }
        if !cmd.subs.is_empty() {
            if is_placeholder(token) {
                break;
            }
            if !cmd.subs.contains(token) {
                out.push(format!(
                    "`{line}`: {token:?} is not a subcommand of `delega {}`",
                    path.join(" ")
                ));
                break;
            }
            path.push(token.to_string());
            if token == "help" {
                break; // the rest are names of other subcommands
            }
        }
        // Otherwise a positional value; not checked.
    }
}

// ---- the check ----

/// Everything in `text` that this build does not offer, one message each.
/// `skill` is the skill's name; only `delegate` has a tool table.
fn problems(text: &str, skill: &str, known: &Known) -> Vec<String> {
    let (spans, fences, mut out) = tokenize(text);
    let longs: BTreeSet<&String> = known.cmds.values().flat_map(|c| c.longs.keys()).collect();
    let shorts: BTreeSet<char> = known
        .cmds
        .values()
        .flat_map(|c| c.shorts.keys().copied())
        .collect();

    for span in &spans {
        if span.starts_with("delega ") {
            check_command_line(span, known, &mut out);
        } else if span.starts_with('-') {
            for token in span.split_whitespace() {
                if let Some((long, name, _)) = flag_parts(token) {
                    let exists = if long {
                        longs.contains(&name)
                    } else {
                        name.chars().next().is_some_and(|c| shorts.contains(&c))
                    };
                    if !exists {
                        out.push(format!(
                            "flag {token} in `{span}` is not an option of any command"
                        ));
                    }
                }
            }
        } else if is_dotted_identifier(span) {
            let first = span.split('.').next().unwrap_or("");
            let top = has_property(&known.request, first) || has_property(&known.result, first);
            if top && !has_property(&known.request, span) && !has_property(&known.result, span) {
                out.push(format!("`{span}` is not a request or result field"));
            }
        } else if is_snake(span) && !known.words.contains(span.as_str()) {
            out.push(format!(
                "`{span}` is not a field, value, tool, argument or command of this build"
            ));
        }
    }
    for lines in &fences {
        for line in lines {
            let line = line.trim().trim_start_matches("$ ");
            if line.starts_with("delega ") {
                check_command_line(line, known, &mut out);
            }
        }
    }

    for cell in table_column(text, "Outcome", 0) {
        for outcome in spans_of(&cell) {
            if !known.outcomes.contains(&outcome) {
                out.push(format!("outcome `{outcome}` is not an Outcome value"));
            }
        }
    }
    for word in outcome_mentions(text) {
        if is_snake(&word) && !known.words.contains(&word) {
            out.push(format!("outcome `{word}` is not an Outcome value"));
        }
    }

    let mut fields = request_field_lists(text);
    for cell in table_column(text, "Request field", 1) {
        // A cell reads `permissions.mode` = `read`; only the first span is the field.
        fields.extend(spans_of(&cell).into_iter().take(1));
    }
    for field in fields {
        if !has_property(&known.request, &field) {
            out.push(format!(
                "request field `{field}` is not in the request schema"
            ));
        }
    }

    if skill == "delegate" {
        for cell in table_column(text, "Tool", 0) {
            for tool in spans_of(&cell) {
                if !MCP_TOOLS.iter().any(|(name, _)| *name == tool) {
                    out.push(format!("MCP tool `{tool}` is not one of the seven tools"));
                }
            }
        }
    }
    out
}

#[test]
fn every_embedded_skill_names_only_what_this_build_offers() {
    let known = known();
    for skill in delega_core::skills::catalog() {
        let found = problems(skill.content(), skill.name(), &known);
        assert!(found.is_empty(), "skill {}: {found:#?}", skill.name());
    }
}

#[test]
fn the_scan_finds_the_things_it_claims_to_check_in_the_real_skills() {
    // Guards against a scan that passes because it found nothing to check.
    let known = known();
    assert!(
        known
            .cmds
            .contains_key(&vec!["skill".to_string(), "show".to_string()])
    );
    let run = delega_core::skills::find("run").expect("run").content();
    let (spans, fences, bad) = tokenize(run);
    assert!(bad.is_empty(), "{bad:?}");
    assert!(fences.is_empty());
    assert!(spans.iter().filter(|s| s.starts_with("--")).count() >= 10);
    assert!(spans.iter().any(|s| s.starts_with("delega run")));
    assert_eq!(table_column(run, "Outcome", 0).len(), 5);
    assert!(table_column(run, "Request field", 1).len() >= 5);
    let delegate = delega_core::skills::find("delegate")
        .expect("delegate")
        .content();
    assert_eq!(table_column(delegate, "Tool", 0).len(), 7);
    assert!(request_field_lists(delegate).len() >= 10);
}

/// The problems for `text` read as the `delegate` skill, which has every check.
fn found(text: &str) -> Vec<String> {
    problems(text, "delegate", &known())
}

/// `text` has exactly one problem and it contains `needle`.
fn only(text: &str, needle: &str) {
    let all = found(text);
    assert_eq!(all.len(), 1, "expected one problem for {text:?}: {all:#?}");
    assert!(all[0].contains(needle), "{needle} not in {all:#?}");
}

#[test]
fn a_flag_of_another_subcommand_is_reported() {
    only("Run `delega version --dry-run`.", "--dry-run");
}

#[test]
fn a_subcommand_that_does_not_exist_is_reported_at_any_depth() {
    only("Run `delega skill install`.", "install");
    only("Run `delega show run`.", "\"show\"");
    only("Run `delega frobnicate`.", "frobnicate");
}

#[test]
fn a_short_flag_that_does_not_exist_is_reported() {
    only("Pass `-Z` to it.", "-Z");
    only("Run `delega run -Z x`.", "-Z");
}

#[test]
fn a_misspelled_bare_field_is_reported() {
    only("Check `provider_notez`.", "provider_notez");
}

#[test]
fn a_dotted_field_missing_from_the_schema_is_reported() {
    only("Check `permissions.nothing`.", "permissions.nothing");
    only("Check `error.nothing`.", "error.nothing");
}

#[test]
fn a_value_outside_a_flags_possible_values_is_reported() {
    only("Run `delega run --effort extreme`.", "extreme");
    only("Run `delega run --effort=extreme`.", "extreme");
    only("Run `delega run --provider gemini`.", "gemini");
}

#[test]
fn an_outcome_that_is_not_an_outcome_is_reported() {
    let table = "| Outcome | Meaning |\n| --- | --- |\n| `completed` | ok |\n| `exploded` | no |\n";
    let all = found(table);
    assert!(
        all.iter().any(|p| p.contains("outcome `exploded`")),
        "{all:#?}"
    );
    assert!(!all.iter().any(|p| p.contains("`completed`")), "{all:#?}");
    assert!(
        found("The outcome `vanished` happens.")
            .iter()
            .any(|p| p.contains("outcome `vanished`"))
    );
}

#[test]
fn a_tool_that_is_not_an_mcp_tool_is_reported_only_in_the_delegate_skill() {
    let table = "| Tool | Arguments |\n| --- | --- |\n| `start` | none |\n| `teleport` | none |\n";
    assert!(
        found(table)
            .iter()
            .any(|p| p.contains("MCP tool `teleport`"))
    );
    let other = problems(table, "run", &known());
    assert!(!other.iter().any(|p| p.contains("MCP tool")), "{other:#?}");
}

#[test]
fn an_unbalanced_backtick_is_a_failure_and_does_not_hide_the_rest() {
    let all = found("A stray ` here.\n\nThen `delega run --bogus-flag` follows.\n");
    assert!(all.iter().any(|p| p.contains("unbalanced")), "{all:#?}");
    assert!(all.iter().any(|p| p.contains("--bogus-flag")), "{all:#?}");
    let unclosed = found("```sh\ndelega version\n");
    assert!(
        unclosed.iter().any(|p| p.contains("never closed")),
        "{unclosed:#?}"
    );
}

#[test]
fn a_fenced_command_line_is_checked_like_a_span() {
    only("```sh\ndelega version --dry-run\n```\n", "--dry-run");
    assert!(found("```sh\ndelega run --effort high -- --foo\n```\n").is_empty());
}

#[test]
fn text_that_names_only_real_things_passes() {
    for fine in [
        "Run `git diff --stat` first.",
        "Run `delega run --provider claude -- --foo --bar`.",
        "Open `result.json`, `example.com` or `0.1.0`.",
        "See `delega help run`.",
        "Run `delega run --effort high --timeout=20m -p hi`.",
        "Run `delega skill show run --output json`.",
        "Use `--effort` or `-C`.",
        "Check `permissions.mode`, `error.kind` and `provider_notes`.",
        "States are `queued` and `idle`; call `wait` with `runs`.",
        "The outcome `text` field is data.",
        "```sh\ndelega schema request | head\ngit log --oneline\n```\n",
    ] {
        let all = found(fine);
        assert!(all.is_empty(), "{fine:?}: {all:#?}");
    }
}

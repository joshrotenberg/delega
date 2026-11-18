//! A skill must not name something this build lacks (docs/api.md, Skills).
//!
//! Each embedded skill is scanned for backticked tokens, and every flag,
//! command, outcome, request field and MCP tool it names is checked against
//! the build: flags and commands against the real `--help` output, outcomes
//! against `Outcome`, fields against the embedded schemas.

use std::collections::BTreeSet;
use std::process::Command;

use delega_core::contract::outcome::Outcome;
use delega_core::contract::schema::{REQUEST_SCHEMA, RESULT_SCHEMA};
use serde_json::Value;

/// The MCP tool names. This moves to the MCP crate, and is read from there,
/// when the server exists.
const MCP_TOOLS: [&str; 7] = [
    "start",
    "follow_up",
    "wait",
    "read",
    "list",
    "interrupt",
    "close",
];

/// What this build offers.
struct Known {
    /// Every `--flag` of the program or any subcommand.
    flags: BTreeSet<String>,
    /// Every subcommand name at any depth.
    commands: BTreeSet<String>,
    outcomes: BTreeSet<String>,
    request: Value,
    result: Value,
}

fn help(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_delega"))
        .args(args)
        .arg("--help")
        .output()
        .expect("run delega --help");
    assert!(out.status.success(), "{args:?} --help failed");
    String::from_utf8(out.stdout).expect("utf-8 help")
}

/// Collect flags and subcommand names from the help of `path`, recursing into
/// the subcommands its `Commands:` section lists.
fn walk_help(path: &[&str], flags: &mut BTreeSet<String>, commands: &mut BTreeSet<String>) {
    let text = help(path);
    flags.extend(flag_tokens(&text));
    let mut in_commands = false;
    let mut names = Vec::new();
    for line in text.lines() {
        if line.starts_with("Commands:") {
            in_commands = true;
        } else if line.is_empty() || !line.starts_with(' ') {
            in_commands = false;
        } else if in_commands
            && let Some(name) = line.split_whitespace().next()
            && name != "help"
        {
            names.push(name.to_string());
        }
    }
    for name in names {
        commands.insert(name.clone());
        let mut next = path.to_vec();
        next.push(&name);
        walk_help(&next, flags, commands);
    }
}

fn known() -> Known {
    let mut flags = BTreeSet::new();
    let mut commands = BTreeSet::new();
    walk_help(&[], &mut flags, &mut commands);
    let outcomes = Outcome::ALL
        .iter()
        .map(|outcome| {
            serde_json::to_value(outcome)
                .expect("serialize outcome")
                .as_str()
                .expect("outcome is a string")
                .to_string()
        })
        .collect();
    Known {
        flags,
        commands,
        outcomes,
        request: serde_json::from_str(REQUEST_SCHEMA).expect("request schema"),
        result: serde_json::from_str(RESULT_SCHEMA).expect("result schema"),
    }
}

/// Every `--long-flag` inside `text`.
fn flag_tokens(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        let at_start = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'-');
        if at_start && bytes[i] == b'-' && bytes[i + 1] == b'-' && bytes[i + 2].is_ascii_lowercase()
        {
            let end = bytes[i + 2..]
                .iter()
                .position(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-'))
                .map_or(bytes.len(), |n| i + 2 + n);
            found.push(text[i..end].to_string());
            i = end;
        } else {
            i += 1;
        }
    }
    found
}

/// The inline code spans of a document, outside fenced blocks.
fn spans(text: &str) -> Vec<String> {
    let mut prose = String::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            prose.push_str(line);
            prose.push('\n');
        }
    }
    prose
        .split('`')
        .skip(1)
        .step_by(2)
        .map(|span| span.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// The cells of one table row.
fn cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// The rows of every table whose header's `column` cell equals `header`, as
/// that column's cell text.
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

/// The code spans inside one cell or snippet.
fn unquote(cell: &str) -> Vec<String> {
    spans(&format!("{cell}\n"))
}

/// Spans in the list that follows "request fields ...:" in a paragraph.
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
        out.extend(spans(&list[..end]));
    }
    out
}

/// Spans written as `outcome `X``.
fn outcome_mentions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices("outcome `") {
        let rest = &text[at + "outcome `".len()..];
        if let Some(end) = rest.find('`') {
            out.push(rest[..end].to_string());
        }
    }
    out
}

/// Resolve `$ref` and `allOf`/`anyOf`/`oneOf` to the schemas that describe a
/// value, so `properties` can be followed through them.
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

/// Whether `path` (`a` or `a.b`) is a property in `root`.
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

fn is_field_path(span: &str) -> bool {
    span.contains('.')
        && span.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// Everything in `text` that this build does not offer, one message each.
fn problems(text: &str, known: &Known) -> Vec<String> {
    let mut out = Vec::new();

    for span in spans(text) {
        for flag in flag_tokens(&span) {
            if !known.flags.contains(&flag) {
                out.push(format!(
                    "flag {flag} in `{span}` is not an option of any command"
                ));
            }
        }
        if let Some(rest) = span.strip_prefix("delega ") {
            let word = rest.split_whitespace().next().unwrap_or("");
            if !word.is_empty()
                && word.bytes().all(|b| b.is_ascii_lowercase())
                && !known.commands.contains(word)
            {
                out.push(format!("command `{span}` names no subcommand {word}"));
            }
        }
        // A dotted path names a request or a result field.
        if is_field_path(&span)
            && !has_property(&known.request, &span)
            && !has_property(&known.result, &span)
        {
            out.push(format!("`{span}` is not a request or result field"));
        }
    }

    for cell in table_column(text, "Outcome", 0) {
        for outcome in unquote(&cell) {
            if !known.outcomes.contains(&outcome) {
                out.push(format!("outcome `{outcome}` is not an Outcome value"));
            }
        }
    }
    for outcome in outcome_mentions(text) {
        if !known.outcomes.contains(&outcome) {
            out.push(format!("outcome `{outcome}` is not an Outcome value"));
        }
    }

    let mut fields = request_field_lists(text);
    for cell in table_column(text, "Request field", 1) {
        // A cell reads `permissions.mode` = `read`; only the first span is the field.
        fields.extend(unquote(&cell).into_iter().take(1));
    }
    for field in fields {
        if !has_property(&known.request, &field) {
            out.push(format!(
                "request field `{field}` is not in the request schema"
            ));
        }
    }

    for cell in table_column(text, "Tool", 0) {
        for tool in unquote(&cell) {
            if !MCP_TOOLS.contains(&tool.as_str()) {
                out.push(format!("MCP tool `{tool}` is not one of {MCP_TOOLS:?}"));
            }
        }
    }

    out
}

#[test]
fn every_embedded_skill_names_only_what_this_build_offers() {
    let known = known();
    for skill in delega_core::skills::catalog() {
        let found = problems(skill.content(), &known);
        assert!(found.is_empty(), "skill {}: {found:#?}", skill.name());
    }
}

#[test]
fn the_scan_sees_the_flags_fields_and_tools_the_skills_really_use() {
    // Guards against a scan that passes because it finds nothing to check.
    let known = known();
    assert!(known.flags.contains("--timeout"), "{:?}", known.flags);
    assert!(known.flags.contains("--output"));
    assert!(known.commands.contains("show"));
    let run = delega_core::skills::find("run").expect("run").content();
    assert!(flag_tokens(run).len() >= 10);
    assert!(table_column(run, "Request field", 1).len() >= 5);
    assert!(table_column(run, "Outcome", 0).len() == 5);
    let delegate = delega_core::skills::find("delegate")
        .expect("delegate")
        .content();
    assert!(table_column(delegate, "Tool", 0).len() == 7);
    assert!(request_field_lists(delegate).len() >= 10);
}

#[test]
fn the_check_reports_a_skill_that_names_what_the_build_lacks() {
    let bad = "\
Run `delega run --nonexistent-flag` or `delega frobnicate`.

`start` takes the same request fields as a run: `provider`, `bogus_field`,
`prompt`. Then the rest.

The outcome `vanished` is not real.

| Tool | Arguments |
| --- | --- |
| `start` | none |
| `teleport` | none |

| Outcome | Meaning |
| --- | --- |
| `completed` | fine |
| `exploded` | not fine |

| Flag | Request field |
| --- | --- |
| `--read` | `permissions.mode` |

| Flag | Request field |
| --- | --- |
| `--x` | `permissions.nothing` |

See `error.nothing`.
";
    let found = problems(bad, &known()).join("\n");
    for expected in [
        "--nonexistent-flag",
        "frobnicate",
        "`bogus_field`",
        "`vanished`",
        "`teleport`",
        "`exploded`",
        "`permissions.nothing`",
        "`error.nothing`",
    ] {
        assert!(found.contains(expected), "missing {expected} in:\n{found}");
    }
    for fine in ["`provider`", "`completed`", "`start`", "`permissions.mode`"] {
        assert!(!found.contains(fine), "{fine} wrongly reported:\n{found}");
    }
}

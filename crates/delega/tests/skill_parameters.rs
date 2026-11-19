//! A skill must not name something this build lacks (docs/api.md, Skills).
//!
//! What is checked, for every embedded skill:
//!
//! - Inline code spans and fenced blocks are found by the markdown rules for
//!   backtick runs. A span or fence that is never closed is a failure.
//! - A span or fenced line that starts with `delega ` is split like a shell
//!   (quotes are honored) and read as a command line. The subcommand path must
//!   exist, nested ones included. Every flag before a `--` must belong to that
//!   subcommand. The value of a flag that takes one is consumed, and must be
//!   one of the possible values when the flag has a fixed set. A positional
//!   with a fixed set of values must be one of them, and the name after
//!   `delega skill show` must be a skill in the catalog (a placeholder such as
//!   `<name>` or `NAME` is accepted). Tokens after `--` are provider options
//!   and are not checked.
//! - A span that begins with a flag (`--timeout 20m`, `-C`) names a flag that
//!   exists on some subcommand, and its value is checked when the flag has a
//!   fixed set of values.
//! - A dotted span whose first part is a top-level request or result field
//!   (`permissions.mode`) must exist in the schema, following `$ref`s, down to
//!   a property with no declared sub-properties (`structured.answer` is
//!   accepted). Dotted spans that start with anything else (`result.json`,
//!   `0.1.0`) are ignored.
//! - A bare snake_case span (`timeout_secs`) must be a schema property, an
//!   enum value, an MCP tool or argument, a subcommand or skill name, or be in
//!   `OTHER_WORDS`.
//! - Table cells in a column headed `Outcome` must be `Outcome` values, and so
//!   must a snake_case word written as "outcome `word`" unless it is any word
//!   this build knows (a field, tool, enum value or command), which is what
//!   keeps "the outcome `text`" from being flagged.
//! - Table cells in a column headed `Request field` start with a request field.
//!   Spans after "request fields ...:" are request fields.
//! - In the `delegate` skill, the first column of a table headed `Tool` holds
//!   MCP tool names, and the second, headed `Arguments`, names only arguments of
//!   that row's tool. The tools and their arguments are read from the tool
//!   schemas the server generates (`delega_mcp::tool_schemas`), not from a list
//!   kept here, and the names in what a tool returns are read from its output
//!   schema, so a bare word naming a tool, an argument or a field of a result
//!   must exist there.
//!
//! What is not checked: prose without backticks, whether a described behavior
//! is true, spans that mix several of these shapes (such as `detail: "full"`),
//! the words after a flag that takes free text, positional values beyond those
//! listed above, command lines that do not start with `delega` (after an
//! environment assignment or a pipe, for example), anything in a command line
//! after the first `|`, `;`, `&&` or `>`, exit status numbers, and prose that
//! is not in backticks.

use std::collections::{BTreeMap, BTreeSet};

use delega_core::contract::outcome::Outcome;
use delega_core::contract::schema::{REQUEST_SCHEMA, RESULT_SCHEMA};
use serde_json::Value;

/// One MCP tool as the server lists it, read from the generated tool schemas
/// (`delega_mcp::tool_schemas`), not from a list kept here.
struct ToolInfo {
    /// The arguments: the properties of the input schema.
    args: BTreeSet<String>,
}

/// Bare words a skill may put in code that are not schema properties, enum
/// values, tools, arguments or command names.
const OTHER_WORDS: [&str; 4] = [
    // Worker states, from the States rule in docs/api.md.
    "queued", "running", "idle", "closed",
];

/// A flag of one command.
#[derive(Default, Clone)]
struct Flag {
    /// Whether the flag is followed by a value.
    takes_value: bool,
    /// The possible values; empty when free-form.
    values: Vec<String>,
}

/// One command in the clap tree.
#[derive(Default)]
struct Cmd {
    longs: BTreeMap<String, Flag>,
    shorts: BTreeMap<char, Flag>,
    subs: BTreeSet<String>,
    /// Possible values of each positional, in order; empty when free-form.
    positionals: Vec<Vec<String>>,
}

struct Known {
    /// Commands by path from the root; the root is the empty path.
    cmds: BTreeMap<Vec<String>, Cmd>,
    outcomes: BTreeSet<String>,
    request: Value,
    result: Value,
    skills: BTreeSet<String>,
    /// The MCP tools by name.
    tools: BTreeMap<String, ToolInfo>,
    /// Every flag on any command, merged: values are the union.
    longs: BTreeMap<String, Flag>,
    shorts: BTreeMap<char, Flag>,
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
        if arg.get_long().is_none() && arg.get_short().is_none() {
            info.positionals.push(values);
            continue;
        }
        let flag = Flag {
            takes_value: arg.get_action().takes_values(),
            values,
        };
        if let Some(long) = arg.get_long() {
            info.longs.insert(long.to_string(), flag.clone());
        }
        if let Some(short) = arg.get_short() {
            info.shorts.insert(short, flag);
        }
    }
    // A hidden subcommand is not part of the surface a skill may name.
    for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
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

fn merge(into: &mut Flag, from: &Flag) {
    into.takes_value |= from.takes_value;
    for value in &from.values {
        if !into.values.contains(value) {
            into.values.push(value.clone());
        }
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
    let mut tools = BTreeMap::new();
    for schema in delega_mcp::tool_schemas() {
        let args: BTreeSet<String> = schema.input["properties"]
            .as_object()
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default();
        words.insert(schema.name.clone());
        words.extend(args.iter().cloned());
        // What a tool returns: the names in its result a skill may describe.
        schema_words(&schema.output, &mut words);
        tools.insert(schema.name, ToolInfo { args });
    }
    words.extend(OTHER_WORDS.iter().map(|w| w.to_string()));
    for info in cmds.values() {
        words.extend(info.subs.iter().cloned());
        words.extend(info.longs.values().flat_map(|f| f.values.iter().cloned()));
    }
    let skills: BTreeSet<String> = delega_core::skills::catalog()
        .iter()
        .map(|s| s.name().to_string())
        .collect();
    words.extend(skills.iter().cloned());
    let mut longs: BTreeMap<String, Flag> = BTreeMap::new();
    let mut shorts: BTreeMap<char, Flag> = BTreeMap::new();
    for info in cmds.values() {
        for (name, flag) in &info.longs {
            merge(longs.entry(name.clone()).or_default(), flag);
        }
        for (name, flag) in &info.shorts {
            merge(shorts.entry(*name).or_default(), flag);
        }
    }
    Known {
        cmds,
        outcomes,
        request,
        result,
        skills,
        tools,
        longs,
        shorts,
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

/// The cells of every row of every table whose first two headers are `first`
/// and `second`.
fn table_rows(text: &str, first: &str, second: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let header = cells(line);
        if !line.starts_with('|')
            || header.first().map(String::as_str) != Some(first)
            || header.get(1).map(String::as_str) != Some(second)
        {
            continue;
        }
        lines.next(); // the separator row
        while let Some(row) = lines.peek().filter(|l| l.starts_with('|')) {
            out.push(cells(row));
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
    for (index, segment) in path.split('.').enumerate() {
        // A property with no declared sub-properties is free-form: anything
        // below it is the caller's data.
        if index > 0
            && current.iter().all(|s| {
                variants(root, s)
                    .iter()
                    .all(|v| v.get("properties").is_none())
            })
        {
            return true;
        }
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

/// Split a line into words like a POSIX shell: whitespace separates, single
/// and double quotes group, and a backslash escapes outside single quotes.
fn shell_split(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => word.push(c),
        }
    }
    if started || !word.is_empty() {
        words.push(word);
    }
    words
}

fn check_value(context: &str, flag: &str, value: &str, values: &[String], out: &mut Vec<String>) {
    if !values.is_empty() && !is_placeholder(value) && !values.iter().any(|v| v == value) {
        out.push(format!(
            "{context}: {value} is not a value of {flag} ({})",
            values.join(", ")
        ));
    }
}

/// Check one command line that starts with `delega`.
fn check_command_line(line: &str, known: &Known, out: &mut Vec<String>) {
    let context = format!("`{line}`");
    let mut tokens = shell_split(line).into_iter();
    tokens.next(); // delega
    let mut path: Vec<String> = Vec::new();
    let mut positional = 0;
    while let Some(token) = tokens.next() {
        if matches!(token.as_str(), "|" | ";" | "&&" | ">" | ">>" | "<" | "--") {
            break; // after `--` come provider options
        }
        let cmd = &known.cmds[&path];
        if let Some((long, name, inline)) = flag_parts(&token) {
            let flag = if long {
                cmd.longs.get(&name)
            } else {
                name.chars().next().and_then(|c| cmd.shorts.get(&c))
            };
            let shown = if long {
                format!("--{name}")
            } else {
                format!("-{name}")
            };
            let Some(flag) = flag else {
                let at = if path.is_empty() {
                    "delega".to_string()
                } else {
                    format!("delega {}", path.join(" "))
                };
                out.push(format!("{context}: {shown} is not an option of `{at}`"));
                continue;
            };
            if flag.takes_value {
                // The value belongs to the flag and is not read as anything else.
                match inline.or_else(|| tokens.next()) {
                    Some(value) => check_value(&context, &shown, &value, &flag.values, out),
                    None => out.push(format!("{context}: {shown} needs a value")),
                }
            }
            continue;
        }
        if !cmd.subs.is_empty() {
            if is_placeholder(&token) {
                break;
            }
            if !cmd.subs.contains(&token) {
                out.push(format!(
                    "{context}: {token:?} is not a subcommand of `delega {}`",
                    path.join(" ")
                ));
                break;
            }
            path.push(token.clone());
            if token == "help" {
                break; // the rest are names of other subcommands
            }
            continue;
        }
        if let Some(values) = cmd.positionals.get(positional) {
            check_value(&context, "this argument", &token, values, out);
            let is_skill_name = path == ["skill", "show"];
            if is_skill_name
                && !is_placeholder(&token)
                && !token.starts_with('<')
                && !known.skills.contains(&token)
            {
                out.push(format!("{context}: {token} is not a skill in the catalog"));
            }
        }
        positional += 1;
    }
}

// ---- the check ----

/// Everything in `text` that this build does not offer, one message each.
/// `skill` is the skill's name; only `delegate` has a tool table.
fn problems(text: &str, skill: &str, known: &Known) -> Vec<String> {
    let (spans, fences, mut out) = tokenize(text);

    for span in &spans {
        if span.starts_with("delega ") {
            check_command_line(span, known, &mut out);
        } else if span.starts_with('-') {
            let mut tokens = shell_split(span).into_iter();
            while let Some(token) = tokens.next() {
                let Some((long, name, inline)) = flag_parts(&token) else {
                    continue;
                };
                let flag = if long {
                    known.longs.get(&name)
                } else {
                    name.chars().next().and_then(|c| known.shorts.get(&c))
                };
                let Some(flag) = flag else {
                    out.push(format!(
                        "flag {token} in `{span}` is not an option of any command"
                    ));
                    continue;
                };
                if flag.takes_value && !flag.values.is_empty() {
                    let value = inline.or_else(|| tokens.next());
                    if let Some(value) = value {
                        check_value(&format!("`{span}`"), &token, &value, &flag.values, &mut out);
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
                "`{span}` is not a field, value, tool, argument or command of this build; \
                 if it is a legitimate other word, add it to OTHER_WORDS with a comment"
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
                if !known.tools.contains_key(&tool) {
                    out.push(format!("MCP tool `{tool}` is not a tool of this build"));
                }
            }
        }
        // Each argument named in a tool's row is an argument of that tool.
        for row in table_rows(text, "Tool", "Arguments") {
            let Some(tool) = row.first().and_then(|c| spans_of(c).into_iter().next()) else {
                continue;
            };
            let Some(info) = known.tools.get(&tool) else {
                continue; // reported above
            };
            for arg in row.get(1).map(|c| spans_of(c)).unwrap_or_default() {
                if is_snake(&arg) && !info.args.contains(&arg) {
                    out.push(format!(
                        "`{arg}` is not an argument of the MCP tool `{tool}`"
                    ));
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
    assert_eq!(table_column(delegate, "Tool", 0).len(), known.tools.len());
    assert_eq!(known.tools.len(), 7);
    assert!(table_rows(delegate, "Tool", "Arguments").len() == 7);
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

#[test]
fn quoted_values_are_one_word_and_flag_values_are_not_read_as_flags() {
    assert!(found("Run `delega run --allow-command \"cargo test --workspace\" -p hi`.").is_empty());
    assert!(found("Run `delega run --allow-command 'git diff --stat' --write`.").is_empty());
    // The value after a value flag is consumed, so a dash word there is a value.
    assert!(found("Run `delega run --name -odd`.").is_empty());
    // A quoted flag-like word is still a real flag problem when it is not a value.
    only(
        "Run `delega run --write \"--nonexistent\"`.",
        "--nonexistent",
    );
    // A missing value is reported.
    only("Run `delega run --effort`.", "needs a value");
}

#[test]
fn a_flag_leading_span_checks_the_value_of_a_fixed_set_flag() {
    only("Pass `--provider gemini`.", "gemini");
    only("Pass `--output yaml`.", "yaml");
    assert!(found("Pass `--provider claude` or `--output json`.").is_empty());
}

#[test]
fn positionals_are_checked_against_their_possible_values_and_the_catalog() {
    only("Run `delega schema bogus`.", "bogus");
    only("Run `delega skill show nope`.", "not a skill");
    assert!(found("Run `delega schema request`.").is_empty());
    assert!(found("Run `delega skill show run`.").is_empty());
    assert!(found("Run `delega skill show <name>` or `delega skill show NAME`.").is_empty());
}

#[test]
fn an_unknown_bare_word_message_says_how_to_allow_it() {
    let all = found("Check `mystery_word`.");
    assert_eq!(all.len(), 1, "{all:#?}");
    assert!(all[0].contains("OTHER_WORDS"), "{all:#?}");
}

#[test]
fn paths_below_a_free_form_property_are_not_checked() {
    assert!(found("Read `structured.answer` and `answer_schema.type`.").is_empty());
    only(
        "Read `permissions.nothing.deeper`.",
        "permissions.nothing.deeper",
    );
}

#[test]
fn an_argument_the_tool_does_not_have_is_reported() {
    let table = "| Tool | Arguments | Returns |\n| --- | --- | --- |\n\
        | `wait` | `runs`, `workers`, `timeout_secs`, `after` | x |\n\
        | `close` | `worker` | x |\n";
    assert!(found(table).is_empty(), "{:#?}", found(table));
    // `wait` has no `worker`, and `close` has no `runs`.
    let wrong = "| Tool | Arguments | Returns |\n| --- | --- | --- |\n\
        | `wait` | `worker` | x |\n| `close` | `runs` | x |\n";
    let all = found(wrong);
    assert_eq!(all.len(), 2, "{all:#?}");
    assert!(
        all.iter()
            .any(|p| p.contains("`worker`") && p.contains("`wait`"))
    );
    assert!(
        all.iter()
            .any(|p| p.contains("`runs`") && p.contains("`close`"))
    );
    // An argument that exists nowhere is reported twice over: as an unknown
    // word and as no argument of the tool.
    let invented = "| Tool | Arguments | Returns |\n| --- | --- | --- |\n\
        | `read` | `page_size` | x |\n";
    let all = found(invented);
    assert!(
        all.iter().any(|p| p.contains("not an argument")),
        "{all:#?}"
    );
}

#[test]
fn a_tool_the_server_does_not_offer_is_reported_as_a_bare_word_too() {
    // `respond` is a later tool: not built, so a skill may not name it.
    let all = found("Call `respond` to answer.");
    assert_eq!(all.len(), 1, "{all:#?}");
    assert!(all[0].contains("respond"), "{all:#?}");
}

#[test]
fn the_delegate_skill_describes_every_tool_the_server_offers() {
    let known = known();
    let delegate = delega_core::skills::find("delegate")
        .expect("delegate")
        .content();
    let described: BTreeSet<String> = table_column(delegate, "Tool", 0)
        .iter()
        .flat_map(|cell| spans_of(cell))
        .collect();
    let offered: BTreeSet<String> = known.tools.keys().cloned().collect();
    assert_eq!(described, offered);
    // Every argument of every tool is named in that tool's row.
    for row in table_rows(delegate, "Tool", "Arguments") {
        let tool = spans_of(&row[0]).remove(0);
        let named: BTreeSet<String> = spans_of(&row[1]).into_iter().collect();
        let args = &known.tools[&tool].args;
        // `start` has the request fields, which the row names as a group.
        let expected: BTreeSet<String> = if tool == "start" {
            ["profile".to_string()].into()
        } else {
            args.clone()
        };
        assert!(
            expected.is_subset(&named),
            "the row for `{tool}` does not name {:?}",
            expected.difference(&named).collect::<Vec<_>>()
        );
    }
}

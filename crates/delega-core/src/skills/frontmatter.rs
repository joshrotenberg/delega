//! Strict frontmatter parsing for `SKILL.md`.
//!
//! This file is compiled both into the crate and into `build.rs`, so a skill
//! that fails these rules cannot be built into the executable.
//!
//! What is accepted, exactly:
//!
//! - The file starts with a `---` line (LF or CRLF). A byte order mark or any
//!   text before it is an error.
//! - Then one `key: value` per line, up to the first line that is exactly
//!   `---`. Everything after that line is the body and is not inspected, so a
//!   `---` inside the body is fine.
//! - Only the keys `name` and `description` are allowed, each exactly once. An
//!   unknown or duplicate key is an error.
//! - A blank line inside the frontmatter is an error, as is a line without a
//!   `key:` at its start (continuation lines are not supported).
//! - A value is the text after the first `:`, trimmed. It is taken literally.
//!   A value that starts with a quote character is an error: write it
//!   unquoted.
//! - Both values must be non-empty.
//! - `name` uses only `a-z`, `0-9` and `-`, does not start or end with `-`, and
//!   equals the skill's directory name.

/// The name and description from a skill file's frontmatter.
#[derive(Debug)]
pub(super) struct Frontmatter {
    pub name: String,
    pub description: String,
}

/// Why a skill's frontmatter was rejected.
#[derive(Debug)]
pub(super) struct FrontmatterError {
    pub message: String,
}

fn err(message: impl Into<String>) -> FrontmatterError {
    FrontmatterError {
        message: message.into(),
    }
}

/// Checks the skill name rules: `[a-z0-9-]`, non-empty, no leading or trailing
/// `-`.
pub(super) fn check_name(name: &str) -> Result<(), FrontmatterError> {
    if name.is_empty() {
        return Err(err("name is empty"));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
    {
        return Err(err(format!(
            "name {name:?} contains {bad:?}; use only a-z, 0-9 and -"
        )));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(err(format!("name {name:?} must not start or end with -")));
    }
    Ok(())
}

/// Parses the frontmatter of `content`, which belongs to the skill directory
/// `directory`.
pub(super) fn parse(directory: &str, content: &str) -> Result<Frontmatter, FrontmatterError> {
    if content.starts_with('\u{feff}') {
        return Err(err(
            "SKILL.md starts with a byte order mark; remove it so the file starts with ---",
        ));
    }
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or_else(|| err("SKILL.md must start with a --- frontmatter line"))?;

    let mut name = None;
    let mut description = None;
    let mut closed = false;
    for line in rest.lines() {
        if line == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() {
            return Err(err("blank line inside the frontmatter"));
        }
        let (key, value) = line
            .split_once(':')
            .filter(|(key, _)| !key.is_empty() && !key.starts_with(char::is_whitespace))
            .ok_or_else(|| err(format!("malformed frontmatter line: {line:?}")))?;
        let slot = match key {
            "name" => &mut name,
            "description" => &mut description,
            _ => {
                return Err(err(format!(
                    "unknown frontmatter key {key:?}; only name and description are allowed"
                )));
            }
        };
        let value = value.trim();
        if value.starts_with(['"', '\'']) {
            return Err(err(format!(
                "frontmatter {key} is quoted; write the value unquoted"
            )));
        }
        if slot.replace(value.to_string()).is_some() {
            return Err(err(format!("duplicate frontmatter key {key:?}")));
        }
    }
    if !closed {
        return Err(err("frontmatter has no closing --- line"));
    }

    let required = |value: Option<String>, key: &str| match value {
        Some(value) if !value.is_empty() => Ok(value),
        Some(_) => Err(err(format!("frontmatter {key} is empty"))),
        None => Err(err(format!("frontmatter has no {key}"))),
    };
    let name = required(name, "name")?;
    let description = required(description, "description")?;
    check_name(&name)?;
    if name != directory {
        return Err(err(format!(
            "frontmatter name {name:?} does not match its directory {directory:?}"
        )));
    }
    Ok(Frontmatter { name, description })
}

#[cfg(test)]
mod tests {
    use super::parse;

    fn message(content: &str) -> String {
        parse("run", content)
            .expect_err("expected an error")
            .message
    }

    #[test]
    fn a_valid_frontmatter_yields_its_name_and_description() {
        let parsed = parse(
            "run",
            "---\nname: run\ndescription: Does a thing.\n---\n# Body\n",
        )
        .expect("valid");
        assert_eq!(parsed.name, "run");
        assert_eq!(parsed.description, "Does a thing.");
    }

    #[test]
    fn crlf_frontmatter_values_carry_no_carriage_return() {
        let parsed =
            parse("run", "---\r\nname: run\r\ndescription: x y\r\n---\r\nbody").expect("crlf");
        assert_eq!(parsed.name, "run");
        assert_eq!(parsed.description, "x y");
        assert!(!parsed.name.contains('\r') && !parsed.description.contains('\r'));
    }

    #[test]
    fn a_dashes_line_inside_the_body_is_not_parsed() {
        let parsed = parse(
            "run",
            "---\nname: run\ndescription: x\n---\nbody\n---\nnot: frontmatter\n---\n",
        )
        .expect("body is ignored");
        assert_eq!(parsed.description, "x");
    }

    #[test]
    fn a_file_without_frontmatter_is_rejected() {
        assert!(message("# Just a body\n").contains("must start with"));
    }

    #[test]
    fn a_byte_order_mark_is_rejected() {
        assert!(
            message("\u{feff}---\nname: run\ndescription: x\n---\n").contains("byte order mark")
        );
    }

    #[test]
    fn unclosed_frontmatter_is_rejected() {
        assert!(message("---\nname: run\ndescription: x\n").contains("no closing"));
    }

    #[test]
    fn a_missing_name_is_rejected() {
        assert!(message("---\ndescription: x\n---\n").contains("no name"));
    }

    #[test]
    fn a_missing_description_is_rejected() {
        assert!(message("---\nname: run\n---\n").contains("no description"));
    }

    #[test]
    fn an_empty_name_is_rejected() {
        assert!(message("---\nname:  \ndescription: x\n---\n").contains("name is empty"));
    }

    #[test]
    fn an_empty_description_is_rejected() {
        assert!(message("---\nname: run\ndescription:\n---\n").contains("description is empty"));
    }

    #[test]
    fn a_name_that_differs_from_its_directory_is_rejected() {
        assert!(message("---\nname: other\ndescription: x\n---\n").contains("does not match"));
    }

    #[test]
    fn a_line_without_a_colon_is_rejected() {
        assert!(message("---\nname run\n---\n").contains("malformed"));
    }

    #[test]
    fn a_duplicate_key_is_rejected() {
        assert!(message("---\nname: run\nname: run\ndescription: x\n---\n").contains("duplicate"));
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        assert!(
            message("---\nname: run\ndescription: x\nlicense: MIT\n---\n")
                .contains("unknown frontmatter key")
        );
    }

    #[test]
    fn a_blank_line_inside_the_frontmatter_is_rejected() {
        assert!(message("---\nname: run\n\ndescription: x\n---\n").contains("blank line"));
    }

    #[test]
    fn a_quoted_value_is_rejected_with_advice_to_write_it_unquoted() {
        let double = message("---\nname: run\ndescription: \"x\"\n---\n");
        assert!(double.contains("unquoted"));
        let single = message("---\nname: 'run'\ndescription: x\n---\n");
        assert!(single.contains("unquoted"));
    }

    #[test]
    fn invalid_name_characters_are_rejected() {
        for bad in ["Run", "run_x", "run x", "-run", "run-", "rün"] {
            let content = format!("---\nname: {bad}\ndescription: x\n---\n");
            assert!(
                parse(bad, &content).is_err(),
                "name {bad:?} should be rejected"
            );
        }
        assert!(parse("run-2", "---\nname: run-2\ndescription: x\n---\n").is_ok());
    }
}

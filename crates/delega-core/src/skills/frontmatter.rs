//! Strict frontmatter parsing for `SKILL.md`: a `---` delimited block of
//! `key: value` lines, of which only `name` and `description` are read.

use super::SkillError;

/// The name and description from a skill file's frontmatter.
pub(super) struct Frontmatter {
    pub name: String,
    pub description: String,
}

/// Parses the frontmatter of `content`, which belongs to the skill directory
/// `directory`.
pub(super) fn parse(directory: &str, content: &str) -> Result<Frontmatter, SkillError> {
    let err = |message: String| SkillError {
        skill: directory.to_string(),
        message,
    };
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or_else(|| err("SKILL.md must start with a --- frontmatter line".into()))?;

    let mut name = None;
    let mut description = None;
    let mut closed = false;
    for line in rest.lines() {
        if line == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .filter(|(key, _)| !key.is_empty() && !key.starts_with(char::is_whitespace))
            .ok_or_else(|| err(format!("malformed frontmatter line: {line:?}")))?;
        let slot = match key {
            "name" => &mut name,
            "description" => &mut description,
            _ => continue,
        };
        if slot.replace(value.trim().to_string()).is_some() {
            return Err(err(format!("duplicate frontmatter key {key:?}")));
        }
    }
    if !closed {
        return Err(err("frontmatter has no closing --- line".into()));
    }

    let required = |value: Option<String>, key: &str| match value {
        Some(value) if !value.is_empty() => Ok(value),
        Some(_) => Err(err(format!("frontmatter {key} is empty"))),
        None => Err(err(format!("frontmatter has no {key}"))),
    };
    let name = required(name, "name")?;
    let description = required(description, "description")?;
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
            .err()
            .expect("expected an error")
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
    fn crlf_line_endings_are_accepted() {
        let parsed =
            parse("run", "---\r\nname: run\r\ndescription: x\r\n---\r\nbody").expect("crlf");
        assert_eq!(parsed.name, "run");
    }

    #[test]
    fn a_file_without_frontmatter_is_rejected() {
        assert!(message("# Just a body\n").contains("must start with"));
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
}

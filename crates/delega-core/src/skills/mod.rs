//! The skill catalog: guidance documents embedded in the executable.
//!
//! Each `skills/<name>/SKILL.md` at the repository root is embedded at build
//! time by `build.rs`, which lists the directories and generates the
//! `include_str!` calls. Nothing is read from disk at run time. The CLI, the
//! MCP server and the plugin installer all serve this one catalog.

mod frontmatter;

use std::fmt;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

include!(concat!(env!("OUT_DIR"), "/embedded_skills.rs"));

/// One embedded skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    name: String,
    description: String,
    content: &'static str,
    digest: String,
}

impl Skill {
    fn parse(directory: &str, content: &'static str) -> Result<Self, SkillError> {
        let front = frontmatter::parse(directory, content)?;
        Ok(Self {
            name: front.name,
            description: front.description,
            content,
            digest: hex(&Sha256::digest(content.as_bytes())),
        })
    }

    /// The skill name, equal to its directory name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The one-line description from the frontmatter.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The full `SKILL.md`, exactly as embedded, frontmatter included.
    pub fn content(&self) -> &'static str {
        self.content
    }

    /// The SHA-256 of [`Skill::content`] as lowercase hex.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// The resource URI: `skill://delega/<name>/SKILL.md`.
    pub fn uri(&self) -> String {
        format!("skill://delega/{}/SKILL.md", self.name)
    }
}

/// A skill file that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillError {
    /// The skill directory name.
    pub skill: String,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for SkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "skill {}: {}", self.skill, self.message)
    }
}

impl std::error::Error for SkillError {}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_all(embedded: &[(&str, &'static str)]) -> Result<Vec<Skill>, SkillError> {
    let mut skills = embedded
        .iter()
        .map(|(directory, content)| Skill::parse(directory, content))
        .collect::<Result<Vec<_>, _>>()?;
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

/// All embedded skills, sorted by name.
///
/// Parsing happens once. A skill file that fails strict parsing is a build
/// defect, caught by this crate's tests, so this panics with the error.
pub fn catalog() -> &'static [Skill] {
    static CATALOG: OnceLock<Vec<Skill>> = OnceLock::new();
    CATALOG.get_or_init(|| match parse_all(EMBEDDED) {
        Ok(skills) => skills,
        Err(error) => panic!("embedded skill catalog is invalid: {error}"),
    })
}

/// The skill called `name`, if there is one.
pub fn find(name: &str) -> Option<&'static Skill> {
    catalog().iter().find(|skill| skill.name == name)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    fn skills_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills")
    }

    #[test]
    fn every_embedded_skill_parses() {
        parse_all(EMBEDDED).expect("every embedded skill parses");
    }

    #[test]
    fn the_catalog_has_the_first_skills() {
        assert!(find("run").is_some());
        assert!(find("delegate").is_some());
        assert!(find("missing").is_none());
    }

    #[test]
    fn skill_names_are_unique_and_equal_their_directory_names() {
        let directories: Vec<&str> = EMBEDDED.iter().map(|(directory, _)| *directory).collect();
        let names: Vec<&str> = catalog().iter().map(Skill::name).collect();
        let unique: BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "duplicate skill names");
        let directories: BTreeSet<&str> = directories.into_iter().collect();
        assert_eq!(unique, directories);
    }

    #[test]
    fn the_catalog_is_sorted_by_name() {
        let names: Vec<&str> = catalog().iter().map(Skill::name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn a_skill_uri_is_skill_delega_name_skill_md() {
        let skill = find("run").expect("run");
        assert_eq!(skill.uri(), "skill://delega/run/SKILL.md");
    }

    #[test]
    fn a_digest_is_the_lowercase_hex_sha256_of_the_embedded_bytes() {
        for skill in catalog() {
            let expected = hex(&Sha256::digest(skill.content().as_bytes()));
            assert_eq!(skill.digest(), expected, "{}", skill.name());
            assert_eq!(skill.digest().len(), 64);
            assert!(
                skill
                    .digest()
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
        }
    }

    #[test]
    fn the_digest_function_matches_a_known_sha256_vector() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn embedded_content_equals_the_file_on_disk() {
        for skill in catalog() {
            let path = skills_dir().join(skill.name()).join("SKILL.md");
            let disk = std::fs::read_to_string(&path).expect("read skill file");
            assert_eq!(skill.content(), disk, "{}", skill.name());
        }
    }

    #[test]
    fn the_catalog_lists_exactly_the_directories_under_skills() {
        let on_disk: BTreeSet<String> = std::fs::read_dir(skills_dir())
            .expect("read skills directory")
            .map(|entry| entry.expect("entry"))
            .filter(|entry| entry.file_type().expect("file type").is_dir())
            .map(|entry| entry.file_name().into_string().expect("UTF-8"))
            .collect();
        let embedded: BTreeSet<String> = catalog()
            .iter()
            .map(|skill| skill.name().to_string())
            .collect();
        assert_eq!(embedded, on_disk, "a skills/ directory is not embedded");
    }

    #[test]
    fn no_skill_file_contains_an_em_dash() {
        for skill in catalog() {
            assert!(!skill.content().contains('\u{2014}'), "{}", skill.name());
        }
    }

    #[test]
    fn a_skill_whose_frontmatter_name_differs_from_its_directory_fails_the_catalog() {
        let bad = [("run", "---\nname: other\ndescription: x\n---\n")];
        let error = parse_all(&bad).expect_err("mismatch");
        assert_eq!(error.skill, "run");
        assert!(error.to_string().contains("does not match"));
    }

    #[test]
    fn a_skill_with_malformed_frontmatter_fails_the_catalog() {
        let bad = [("run", "no frontmatter")];
        assert!(parse_all(&bad).is_err());
    }
}

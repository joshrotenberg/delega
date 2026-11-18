//! The skill catalog: guidance documents embedded in the executable.
//!
//! Each `skills/<name>/SKILL.md` at the repository root is embedded at build
//! time by `build.rs`, which lists the directories and generates the
//! `include_str!` calls. Nothing is read from disk at run time. The CLI, the
//! MCP server and the plugin installer all serve this one catalog.
//!
//! `build.rs` validates every skill with the parser in `frontmatter.rs`, which
//! documents exactly what is accepted, so an invalid skill cannot be built in.

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
    pub(crate) fn parse(directory: &str, content: &'static str) -> Result<Self, SkillError> {
        let front = frontmatter::parse(directory, content).map_err(|error| SkillError {
            skill: directory.to_string(),
            message: error.message,
        })?;
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
    if let Some(pair) = skills.windows(2).find(|pair| pair[0].name == pair[1].name) {
        return Err(SkillError {
            skill: pair[0].name.clone(),
            message: "duplicate skill name".into(),
        });
    }
    Ok(skills)
}

/// All embedded skills, sorted by name.
///
/// Parsing happens once. `build.rs` already rejected any skill that fails
/// strict parsing, so the panic is unreachable in a built binary.
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

    fn doc(name: &str) -> &'static str {
        match name {
            "alpha" => "---\nname: alpha\ndescription: a\n---\n",
            "beta" => "---\nname: beta\ndescription: b\n---\n",
            "gamma" => "---\nname: gamma\ndescription: c\n---\n",
            _ => unreachable!(),
        }
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
    fn unsorted_input_comes_back_sorted_by_name() {
        let input = [
            ("gamma", doc("gamma")),
            ("alpha", doc("alpha")),
            ("beta", doc("beta")),
        ];
        let names: Vec<String> = parse_all(&input)
            .expect("parses")
            .iter()
            .map(|skill| skill.name().to_string())
            .collect();
        assert_eq!(names, ["alpha", "beta", "gamma"]);
    }

    #[test]
    fn two_skills_with_the_same_name_are_an_error() {
        let input = [("alpha", doc("alpha")), ("alpha", doc("alpha"))];
        let error = parse_all(&input).expect_err("duplicate");
        assert!(error.message.contains("duplicate"));
    }

    #[test]
    fn a_skill_uri_is_skill_delega_name_skill_md() {
        let skill = find("run").expect("run");
        assert_eq!(skill.uri(), "skill://delega/run/SKILL.md");
    }

    #[test]
    fn a_digest_is_the_pinned_sha256_of_the_exact_bytes() {
        // printf '---\nname: run\ndescription: Does a thing.\n---\nbody\n' | shasum -a 256
        let skill = Skill::parse(
            "run",
            "---\nname: run\ndescription: Does a thing.\n---\nbody\n",
        )
        .expect("parses");
        assert_eq!(
            skill.digest(),
            "d5545b25e17d6ec2a185b947357ec6e9356d9529d46e2703baf23efcf24f8e8a"
        );
    }

    #[test]
    fn the_digest_covers_every_byte_including_line_endings() {
        let lf = Skill::parse("run", "---\nname: run\ndescription: x\n---\n").expect("lf");
        let crlf =
            Skill::parse("run", "---\r\nname: run\r\ndescription: x\r\n---\r\n").expect("crlf");
        assert_ne!(lf.digest(), crlf.digest());
    }

    #[test]
    fn the_catalog_lists_exactly_the_directories_under_skills() {
        let on_disk: BTreeSet<String> = std::fs::read_dir(skills_dir())
            .expect("read skills directory")
            .map(|entry| entry.expect("entry"))
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .filter(|entry| std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()))
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

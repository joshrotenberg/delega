//! Generates the list of embedded skills from the `skills/` directory at the
//! repository root, so adding a skill is one step: add `skills/<name>/SKILL.md`.
//!
//! Each skill is validated here with the same parser the crate uses, so a skill
//! that fails strict parsing stops the build. Entries whose name starts with
//! `.` are skipped. A symlink to a directory is followed.

use std::env;
use std::fs;
use std::path::PathBuf;

#[allow(dead_code)]
#[path = "src/skills/frontmatter.rs"]
mod frontmatter;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let skills = manifest.join("../../skills");
    println!("cargo:rerun-if-changed={}", skills.display());
    println!("cargo:rerun-if-changed=src/skills/frontmatter.rs");

    let mut names = Vec::new();
    for entry in fs::read_dir(&skills).expect("read the skills directory") {
        let entry = entry.expect("read a skills directory entry");
        let name = entry
            .file_name()
            .into_string()
            .expect("skill directory names are UTF-8");
        if name.starts_with('.') {
            continue;
        }
        // metadata follows symlinks, so a linked directory counts as one.
        let is_dir = fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir());
        if !is_dir {
            continue;
        }
        if let Err(error) = frontmatter::check_name(&name) {
            panic!("skills/{name}: invalid directory name: {}", error.message);
        }
        let file = entry.path().join("SKILL.md");
        println!("cargo:rerun-if-changed={}", file.display());
        let content = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("skills/{name}/SKILL.md cannot be read: {error}"));
        if let Err(error) = frontmatter::parse(&name, &content) {
            panic!("skills/{name}/SKILL.md is invalid: {}", error.message);
        }
        names.push(name);
    }
    names.sort();

    let mut out = String::from("pub(super) const EMBEDDED: &[(&str, &str)] = &[\n");
    for name in &names {
        let path = skills.join(name).join("SKILL.md");
        let path = path.canonicalize().expect("canonical skill path");
        out.push_str(&format!(
            "    ({name:?}, include_str!({:?})),\n",
            path.to_str().expect("UTF-8 path")
        ));
    }
    out.push_str("];\n");

    let dest = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("embedded_skills.rs");
    fs::write(dest, out).expect("write embedded_skills.rs");
}

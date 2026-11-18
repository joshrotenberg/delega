//! Generates the list of embedded skills from the `skills/` directory at the
//! repository root, so adding a skill is one step: add `skills/<name>/SKILL.md`.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let skills = manifest.join("../../skills");
    println!("cargo:rerun-if-changed={}", skills.display());

    let mut names = Vec::new();
    for entry in fs::read_dir(&skills).expect("read the skills directory") {
        let entry = entry.expect("read a skills directory entry");
        if !entry.file_type().expect("file type").is_dir() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .expect("skill directory names are UTF-8");
        let file = entry.path().join("SKILL.md");
        println!("cargo:rerun-if-changed={}", file.display());
        assert!(file.is_file(), "skills/{name} has no SKILL.md");
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

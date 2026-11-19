//! `delega init` and `delega uninstall` against fake host CLIs.
//!
//! Every test runs the built binary with an empty environment plus a
//! temporary HOME, a PATH holding only an empty directory, and
//! `DELEGA_CLAUDE_BIN` / `DELEGA_CODEX_BIN` pointing at copies of the fake host
//! script (`tests/fakes/fake-host.sh`) that record each call. The real `claude`
//! and `codex` are never found, and nothing outside the temporary directory is
//! read or written.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const PLUGIN_ID: &str = "delega@delega-local";

struct Rig {
    _tmp: TempDir,
    home: PathBuf,
    /// Holds the fake `claude` and `codex`, their logs and their state.
    hosts: PathBuf,
    empty: PathBuf,
    install: PathBuf,
    claude: bool,
    codex: bool,
    env: Vec<(String, String)>,
}

impl Rig {
    /// A rig with both fake hosts present.
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("temporary directory");
        let root = fs::canonicalize(tmp.path()).expect("resolve");
        let make = |name: &str| {
            let path = root.join(name);
            fs::create_dir(&path).expect("create directory");
            path
        };
        let hosts = make("hosts");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/fake-host.sh");
        for name in ["claude", "codex"] {
            fs::copy(&script, hosts.join(name)).expect("copy fake host");
        }
        let home = make("home");
        Self {
            install: home.join(".local/share/delega"),
            empty: make("empty"),
            home,
            hosts,
            _tmp: tmp,
            claude: true,
            codex: true,
            env: Vec::new(),
        }
    }

    fn only(mut self, claude: bool, codex: bool) -> Self {
        self.claude = claude;
        self.codex = codex;
        self
    }

    fn set(&mut self, name: &str, value: &str) {
        self.env.push((name.to_string(), value.to_string()));
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_delega"));
        command
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.empty);
        if self.claude {
            command.env("DELEGA_CLAUDE_BIN", self.hosts.join("claude"));
        }
        if self.codex {
            command.env("DELEGA_CODEX_BIN", self.hosts.join("codex"));
        }
        for (name, value) in &self.env {
            command.env(name, value);
        }
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run delega")
    }

    /// Run with `--output json`, check the document against the committed
    /// schema, and return it with the exit code.
    fn run_json(&self, args: &[&str]) -> (i32, Value, String) {
        let mut all = args.to_vec();
        all.extend(["--output", "json"]);
        let out = self.run(&all);
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(&out.stdout)));
        let schema = if args[0] == "init" {
            include_str!("../../../schemas/init.schema.json")
        } else {
            include_str!("../../../schemas/uninstall.schema.json")
        };
        let schema: Value = serde_json::from_str(schema).expect("schema");
        let validator = jsonschema::validator_for(&schema).expect("compile schema");
        let errors: Vec<String> = validator
            .iter_errors(&doc)
            .map(|e| format!("{e} at {}", e.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{errors:?} in {doc}");
        assert_eq!(doc["ok"], out.status.code() == Some(0), "{doc}");
        (out.status.code().unwrap_or(-1), doc, text(&out.stderr))
    }

    /// The recorded calls of a host, in order, without the listing calls
    /// when `listings` is false.
    fn log(&self, host: &str, listings: bool) -> Vec<String> {
        fs::read_to_string(self.hosts.join(format!("{host}.log")))
            .unwrap_or_default()
            .lines()
            .filter(|line| listings || !line.ends_with(" list --json"))
            .map(String::from)
            .collect()
    }

    fn no_calls(&self) {
        for host in ["claude", "codex"] {
            assert!(
                !self.hosts.join(format!("{host}.log")).exists(),
                "{host} was called: {:?}",
                self.log(host, true)
            );
        }
    }

    fn state(&self, host: &str, what: &str) -> PathBuf {
        self.hosts.join("state").join(format!("{host}.{what}"))
    }

    fn marker(&self) -> Value {
        read_json(&self.install.join(".delega-install.json"))
    }

    fn dir(&self) -> String {
        self.install.display().to_string()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .expect("JSON file")
}

fn delega_output(args: &[&str]) -> Vec<u8> {
    let out = Command::new(env!("CARGO_BIN_EXE_delega"))
        .args(args)
        .output()
        .expect("run delega");
    assert!(out.status.success());
    out.stdout
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn claude_install(dir: &str) -> Vec<String> {
    vec![
        format!("plugin marketplace add {dir}"),
        format!("plugin install {PLUGIN_ID}"),
        format!("plugin update {PLUGIN_ID} --scope user"),
    ]
}

fn codex_install(dir: &str) -> Vec<String> {
    vec![
        format!("plugin marketplace add {dir}"),
        format!("plugin add {PLUGIN_ID}"),
    ]
}

const CLAUDE_UNINSTALL: [&str; 2] = [
    "plugin uninstall delega@delega-local --scope user",
    "plugin marketplace remove delega-local --scope user",
];
const CODEX_UNINSTALL: [&str; 2] = [
    "plugin remove delega@delega-local",
    "plugin marketplace remove delega-local",
];

// ---- init ----

#[test]
fn dry_run_writes_nothing_and_runs_nothing() {
    let rig = Rig::new();
    let out = rig.run(&["init", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let shown = text(&out.stdout);
    assert!(
        shown.contains("would run: claude plugin marketplace add"),
        "{shown}"
    );
    assert!(
        shown.contains("would run: codex plugin add delega@delega-local"),
        "{shown}"
    );
    assert!(shown.contains("plugin/delega/bin/delega"), "{shown}");
    assert!(shown.contains("Dry run: nothing was written and no host command ran."));
    assert!(!rig.home.join(".local").exists(), "a dry run wrote");
    rig.no_calls();

    let (code, doc, _) = rig.run_json(&["init", "--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(doc["dry_run"], true);
    assert_eq!(doc["hosts"][0]["status"], "would_install");
    assert_eq!(doc["hosts"][0]["commands"][0]["status"], "would_run");
    assert!(!rig.home.join(".local").exists());
    rig.no_calls();
}

#[test]
fn install_for_claude_runs_the_claude_sequence_only() {
    let rig = Rig::new();
    let out = rig.run(&["init", "--host", "claude"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(rig.log("claude", true), claude_install(&rig.dir()));
    assert!(!rig.hosts.join("codex.log").exists());
    assert_eq!(rig.marker()["hosts"], serde_json::json!(["claude"]));
    let shown = text(&out.stdout);
    assert!(shown.contains("Installed delega@delega-local"), "{shown}");
    assert!(
        shown.contains("reconnect plugin:delega:delega from /mcp"),
        "{shown}"
    );
}

#[test]
fn install_for_codex_runs_the_codex_sequence_only() {
    let rig = Rig::new();
    let out = rig.run(&["init", "--host", "codex"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(rig.log("codex", true), codex_install(&rig.dir()));
    assert!(!rig.hosts.join("claude.log").exists());
    assert_eq!(rig.marker()["hosts"], serde_json::json!(["codex"]));
    assert!(text(&out.stdout).contains("start a new Codex session"));
}

#[test]
fn install_for_both_runs_both_sequences() {
    let rig = Rig::new();
    let (code, doc, err) = rig.run_json(&["init"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(rig.log("claude", true), claude_install(&rig.dir()));
    assert_eq!(rig.log("codex", true), codex_install(&rig.dir()));
    assert_eq!(
        rig.marker()["hosts"],
        serde_json::json!(["claude", "codex"])
    );
    assert_eq!(doc["hosts"][0]["status"], "installed");
    assert_eq!(doc["hosts"][1]["status"], "installed");
    assert_eq!(doc["install_dir"], rig.dir());
    // Every host command ran from a neutral directory.
    for host in ["claude", "codex"] {
        let cwd = fs::read_to_string(rig.hosts.join(format!("{host}.cwd"))).unwrap();
        assert!(cwd.lines().all(|line| line == "/"), "{host}: {cwd}");
    }
}

#[test]
fn install_dir_option_and_environment_choose_the_directory() {
    let mut rig = Rig::new();
    let chosen = rig.home.join("elsewhere");
    let out = rig.run(&[
        "init",
        "--host",
        "codex",
        "--install-dir",
        chosen.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(chosen.join(".delega-install.json").exists());
    assert!(!rig.install.exists());

    let from_env = rig.home.join("from-env");
    rig.set("DELEGA_INSTALL_HOME", from_env.to_str().unwrap());
    let out = rig.run(&["init", "--host", "codex"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(from_env.join(".delega-install.json").exists());
}

#[test]
fn a_host_that_is_not_found_is_reported_and_skipped() {
    let rig = Rig::new().only(true, false);
    let (code, doc, err) = rig.run_json(&["init"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(doc["hosts"][1]["host"], "codex");
    assert_eq!(doc["hosts"][1]["status"], "skipped");
    assert!(
        doc["hosts"][1]["note"]
            .as_str()
            .unwrap()
            .contains("DELEGA_CODEX_BIN")
    );
    assert_eq!(rig.log("claude", true), claude_install(&rig.dir()));
    assert!(!rig.hosts.join("codex.log").exists());
    assert_eq!(rig.marker()["hosts"], serde_json::json!(["claude"]));
}

#[test]
fn a_named_host_that_is_not_found_fails_and_writes_nothing() {
    let rig = Rig::new().only(true, false);
    let (code, doc, err) = rig.run_json(&["init", "--host", "claude", "--host", "codex"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("a host named with --host was not found"),
        "{err}"
    );
    assert_eq!(doc["files"], serde_json::json!([]));
    assert!(!rig.home.join(".local").exists());
    rig.no_calls();
}

#[test]
fn no_host_found_fails_and_writes_nothing() {
    let rig = Rig::new().only(false, false);
    let (code, _, err) = rig.run_json(&["init"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("neither Claude Code nor Codex was found"),
        "{err}"
    );
    assert!(!rig.home.join(".local").exists());
}

#[test]
fn a_foreign_non_empty_directory_is_refused_untouched() {
    let rig = Rig::new();
    fs::create_dir_all(&rig.install).unwrap();
    fs::write(rig.install.join("mine.txt"), "keep").unwrap();
    let (code, doc, err) = rig.run_json(&["init"]);
    assert_eq!(code, 1);
    assert!(err.contains("was not created by delega init"), "{err}");
    assert_eq!(doc["files"], serde_json::json!([]));
    let names: Vec<_> = fs::read_dir(&rig.install)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["mine.txt"]);
    rig.no_calls();
}

#[test]
fn an_invalid_marker_refuses_init() {
    let rig = Rig::new();
    fs::create_dir_all(&rig.install).unwrap();
    fs::write(
        rig.install.join(".delega-install.json"),
        "{\"managed_by\":\"someone\"}",
    )
    .unwrap();
    let (code, _, err) = rig.run_json(&["init"]);
    assert_eq!(code, 1);
    assert!(err.contains("no valid delega ownership marker"), "{err}");
    rig.no_calls();
}

#[test]
fn an_empty_existing_directory_is_reused() {
    let rig = Rig::new();
    fs::create_dir_all(&rig.install).unwrap();
    let out = rig.run(&["init", "--host", "codex"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(rig.install.join("plugin/delega/bin/delega").exists());
}

#[test]
fn the_managed_files_have_the_documented_bytes() {
    let rig = Rig::new();
    let (code, doc, err) = rig.run_json(&["init"]);
    assert_eq!(code, 0, "{err}");
    let plugin = rig.install.join("plugin/delega");

    // The skills are the bytes of `delega skill show`.
    let list: Value =
        serde_json::from_slice(&delega_output(&["skill", "list", "--output", "json"])).unwrap();
    let skills = list["skills"].as_array().unwrap();
    assert!(!skills.is_empty());
    for skill in skills {
        let name = skill["name"].as_str().unwrap();
        let shown = delega_output(&["skill", "show", name]);
        let written = fs::read(plugin.join("skills").join(name).join("SKILL.md")).unwrap();
        assert_eq!(written, shown, "{name}");
    }
    // The identity file is `delega version --output json`.
    assert_eq!(
        fs::read(plugin.join("IDENTITY.json")).unwrap(),
        delega_output(&["version", "--output", "json"])
    );
    // The executable is a copy of the running one, and the plugin version
    // carries its digest.
    let copy = fs::read(plugin.join("bin/delega")).unwrap();
    assert_eq!(copy, fs::read(env!("CARGO_BIN_EXE_delega")).unwrap());
    let digest: String = Sha256::digest(&copy)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let version = format!("{}+{digest}", env!("CARGO_PKG_VERSION"));
    assert_eq!(doc["plugin_version"], version.as_str());
    for manifest in [
        "plugin.json",
        ".claude-plugin/plugin.json",
        "../../.claude-plugin/marketplace.json",
    ] {
        let value = read_json(&plugin.join(manifest));
        let found = value["version"]
            .as_str()
            .or_else(|| value["plugins"][0]["version"].as_str());
        assert_eq!(found, Some(version.as_str()), "{manifest}");
    }
    // Both MCP manifests start the copied executable with `mcp`.
    let claude = read_json(&plugin.join(".mcp.json"));
    // No `cwd`: each host starts the server in its own default directory.
    assert!(claude["mcpServers"]["delega"].get("cwd").is_none());
    assert_eq!(
        claude["mcpServers"]["delega"]["command"],
        "${CLAUDE_PLUGIN_ROOT}/bin/delega"
    );
    assert_eq!(
        claude["mcpServers"]["delega"]["args"],
        serde_json::json!(["mcp"])
    );
    let codex = read_json(&plugin.join("mcp.json"));
    assert!(codex["mcpServers"]["delega"].get("cwd").is_none());
    assert_eq!(codex["mcpServers"]["delega"]["command"], "./bin/delega");
    assert_eq!(
        codex["mcpServers"]["delega"]["args"],
        serde_json::json!(["mcp"])
    );
    // Both marketplaces name delega-local and the plugin source.
    assert_eq!(
        read_json(&rig.install.join(".claude-plugin/marketplace.json"))["name"],
        "delega-local"
    );
    let codex_market = read_json(&rig.install.join(".agents/plugins/marketplace.json"));
    assert_eq!(codex_market["name"], "delega-local");
    assert_eq!(
        codex_market["plugins"][0]["source"]["path"],
        "./plugin/delega"
    );
    // The marker lists every file reported, and the hosts.
    let marker = rig.marker();
    assert_eq!(marker["format_version"], 1);
    assert_eq!(marker["managed_by"], "delega");
    let mut listed: Vec<String> = marker["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap().to_string())
        .collect();
    listed.push(".delega-install.json".into());
    let mut reported: Vec<String> = doc["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap().to_string())
        .collect();
    listed.sort();
    reported.sort();
    assert_eq!(listed, reported);
    for file in &listed {
        assert!(rig.install.join(file).is_file(), "{file}");
    }
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(plugin.join("bin/delega"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
}

#[test]
fn a_repeated_install_updates_files_and_repeats_the_commands() {
    let rig = Rig::new();
    let (_, first, _) = rig.run_json(&["init", "--host", "claude"]);
    let skill = rig.install.join("plugin/delega/skills/run/SKILL.md");
    fs::write(&skill, "stale").unwrap();
    let (code, second, err) = rig.run_json(&["init", "--host", "claude"]);
    assert_eq!(code, 0, "{err}");
    // The same executable gives the same plugin version, and the files are
    // written again.
    assert_eq!(first["plugin_version"], second["plugin_version"]);
    assert_eq!(
        fs::read(&skill).unwrap(),
        delega_output(&["skill", "show", "run"])
    );
    let mut expected = claude_install(&rig.dir());
    expected.extend(claude_install(&rig.dir()));
    assert_eq!(rig.log("claude", true), expected);
    // A later init for the other host keeps the first recorded.
    let out = rig.run(&["init", "--host", "codex"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        rig.marker()["hosts"],
        serde_json::json!(["claude", "codex"])
    );
}

#[test]
fn a_failed_host_command_is_exit_one_and_stops_that_host() {
    let mut rig = Rig::new();
    rig.set("FAKE_FAIL", "update");
    let (code, doc, err) = rig.run_json(&["init"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("installation did not complete for Claude Code"),
        "{err}"
    );
    assert_eq!(rig.log("claude", true), claude_install(&rig.dir()));
    assert_eq!(doc["hosts"][0]["status"], "failed");
    assert_eq!(doc["hosts"][0]["commands"][2]["status"], "failed");
    assert!(
        doc["hosts"][0]["commands"][2]["output"]
            .as_str()
            .unwrap()
            .contains("fake failure")
    );
    assert_eq!(doc["hosts"][1]["status"], "installed");
    // The files stay in place for a retry.
    assert!(rig.install.join(".delega-install.json").exists());
}

#[test]
fn a_failed_command_leaves_the_rest_of_that_host_unrun() {
    let mut rig = Rig::new();
    rig.set("FAKE_FAIL", "install");
    let (code, doc, _) = rig.run_json(&["init", "--host", "claude"]);
    assert_eq!(code, 1);
    assert_eq!(
        rig.log("claude", true),
        claude_install(&rig.dir())[..2].to_vec()
    );
    assert_eq!(doc["hosts"][0]["commands"][2]["status"], "not_run");
}

#[test]
fn usage_errors_exit_two() {
    let rig = Rig::new();
    for args in [
        &["init", "--host", "nope"][..],
        &["init", "--install-dir", "relative/dir"],
        &["uninstall", "--install-dir", "relative/dir"],
        &["init", "--output", "xml"],
        &["uninstall", "--bogus"],
    ] {
        let out = rig.run(args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
    // No HOME and no directory given.
    for command in ["init", "uninstall"] {
        let out = Command::new(env!("CARGO_BIN_EXE_delega"))
            .arg(command)
            .env_clear()
            .env("PATH", &rig.empty)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{command}");
        assert!(text(&out.stderr).contains("HOME is not set"), "{command}");
    }
    rig.no_calls();
}

// ---- uninstall ----

fn installed_for_both() -> Rig {
    let rig = Rig::new();
    let out = rig.run(&["init"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    rig
}

fn listing() -> Vec<String> {
    strings(&["plugin marketplace list --json", "plugin list --json"])
}

#[test]
fn uninstall_removes_both_registrations_and_the_directory() {
    let rig = installed_for_both();
    let (code, doc, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 0, "{err}");
    let mut claude = claude_install(&rig.dir());
    claude.extend(listing());
    claude.extend(CLAUDE_UNINSTALL.map(String::from));
    assert_eq!(rig.log("claude", true), claude);
    let mut codex = codex_install(&rig.dir());
    codex.extend(listing());
    codex.extend(CODEX_UNINSTALL.map(String::from));
    assert_eq!(rig.log("codex", true), codex);
    assert!(!rig.install.exists(), "the directory was kept");
    assert_eq!(doc["directory"], "removed");
    assert_eq!(doc["hosts"][0]["status"], "removed");
    assert!(!rig.state("claude", "plugin").exists());
    assert!(!rig.state("codex", "marketplace").exists());
    for host in ["claude", "codex"] {
        let cwd = fs::read_to_string(rig.hosts.join(format!("{host}.cwd"))).unwrap();
        assert!(cwd.lines().all(|line| line == "/"), "{host}: {cwd}");
    }
}

#[test]
fn uninstall_host_keeps_the_directory_for_the_other_host() {
    let rig = installed_for_both();
    let codex_before = rig.log("codex", true);
    let (code, doc, err) = rig.run_json(&["uninstall", "--host", "claude"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(doc["directory"], "kept");
    assert!(rig.install.join("plugin/delega/bin/delega").exists());
    assert_eq!(rig.marker()["hosts"], serde_json::json!(["codex"]));
    assert_eq!(rig.log("codex", true), codex_before, "codex was touched");
    let claude = rig.log("claude", false);
    assert_eq!(claude[3..], CLAUDE_UNINSTALL.map(String::from));
    // Then the rest, which removes the directory.
    let (code, doc, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(doc["directory"], "removed");
    assert!(!rig.install.exists());
    assert_eq!(
        rig.log("claude", true).len(),
        3 + 2 + 2,
        "claude was called again"
    );
}

#[test]
fn uninstall_host_for_the_only_host_removes_the_directory() {
    let rig = Rig::new();
    assert_eq!(rig.run(&["init", "--host", "codex"]).status.code(), Some(0));
    let (code, doc, err) = rig.run_json(&["uninstall", "--host", "codex"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(doc["directory"], "removed");
    assert!(!rig.install.exists());
}

#[test]
fn uninstall_stops_on_a_foreign_marketplace() {
    let rig = installed_for_both();
    fs::write(rig.state("claude", "marketplace"), "/elsewhere").unwrap();
    let before = rig.log("claude", true).len();
    let (code, doc, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 1);
    assert!(err.contains("another or unverified directory"), "{err}");
    assert_eq!(doc["hosts"][0]["status"], "refused");
    assert!(rig.install.join(".delega-install.json").exists());
    // Only listings ran, and nothing was removed from either host.
    assert_eq!(
        rig.log("claude", true).len(),
        before + 1,
        "only the marketplace listing ran"
    );
    assert!(
        rig.log("claude", false)
            .iter()
            .all(|c| !c.contains("uninstall") && !c.contains("remove"))
    );
    assert!(
        rig.log("codex", false)
            .iter()
            .all(|c| !c.contains("remove"))
    );
    assert!(rig.state("codex", "plugin").exists());
}

#[test]
fn uninstall_stops_on_a_foreign_plugin_registration() {
    let rig = installed_for_both();
    fs::write(rig.state("codex", "plugin"), "/elsewhere/plugin/delega").unwrap();
    let (code, _, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 1);
    assert!(err.contains("cannot be attributed to"), "{err}");
    assert!(rig.install.join("plugin/delega/bin/delega").exists());
    // Verification of every host precedes any removal, so claude is intact too.
    assert!(rig.state("claude", "plugin").exists());
    assert!(
        rig.log("claude", false)
            .iter()
            .all(|c| !c.contains("uninstall"))
    );
}

#[test]
fn uninstall_stops_when_another_plugin_is_installed_from_the_marketplace() {
    let rig = installed_for_both();
    fs::write(rig.state("claude", "extra"), "").unwrap();
    let (code, _, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("another plugin installed from delega-local"),
        "{err}"
    );
    assert!(rig.install.exists());
    assert!(
        rig.log("claude", false)
            .iter()
            .all(|c| !c.contains("uninstall"))
    );
}

#[test]
fn uninstall_stops_on_a_failing_or_unreadable_listing() {
    for (name, value) in [("FAKE_FAIL", "list"), ("FAKE_GARBAGE", "yes")] {
        let mut rig = installed_for_both();
        rig.set(name, value);
        let (code, _, err) = rig.run_json(&["uninstall"]);
        assert_eq!(code, 1, "{name}");
        assert!(err.contains("cannot verify registration"), "{name}: {err}");
        assert!(rig.install.join(".delega-install.json").exists(), "{name}");
        assert!(rig.state("claude", "plugin").exists(), "{name}");
        assert!(
            rig.log("claude", false)
                .iter()
                .all(|c| !c.contains("uninstall")),
            "{name}"
        );
    }
}

#[test]
fn uninstall_keeps_files_when_a_removal_command_fails() {
    let mut rig = installed_for_both();
    rig.set("FAKE_FAIL", "uninstall");
    let (code, doc, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 1);
    assert!(err.contains("removal failed for Claude Code"), "{err}");
    assert!(rig.install.join("plugin/delega/bin/delega").exists());
    assert_eq!(doc["hosts"][0]["status"], "failed");
    assert_eq!(doc["hosts"][0]["commands"][1]["status"], "not_run");
    // Codex was removed and is no longer recorded; claude still is.
    assert_eq!(doc["hosts"][1]["status"], "removed");
    assert_eq!(rig.marker()["hosts"], serde_json::json!(["claude"]));
}

#[test]
fn an_invalid_marker_stops_uninstall() {
    let rig = installed_for_both();
    let marker = rig.install.join(".delega-install.json");
    let good = read_json(&marker);
    let cases: Vec<(&str, Value)> = vec![
        ("managed_by", serde_json::json!("counterpart")),
        ("format_version", serde_json::json!(2)),
        ("hosts", serde_json::json!(["claud"])),
        ("hosts", serde_json::json!(["claude", "claude"])),
        ("files", serde_json::json!(["../outside"])),
        ("files", serde_json::json!(["/etc/passwd"])),
        ("files", serde_json::json!(["a", "a"])),
    ];
    for (field, value) in cases {
        let mut bad = good.clone();
        bad[field] = value;
        fs::write(&marker, serde_json::to_vec(&bad).unwrap()).unwrap();
        let before = rig.log("claude", true).len();
        let (code, _, err) = rig.run_json(&["uninstall"]);
        assert_eq!(code, 1, "{field}");
        assert!(
            err.contains("no valid delega ownership marker"),
            "{field}: {err}"
        );
        assert_eq!(
            rig.log("claude", true).len(),
            before,
            "{field}: a host was called"
        );
        assert!(rig.install.join("plugin/delega/bin/delega").exists());
    }
    fs::write(&marker, "not json").unwrap();
    assert_eq!(rig.run(&["uninstall"]).status.code(), Some(1));
    assert!(rig.install.join("plugin/delega/bin/delega").exists());
}

#[test]
fn an_unmanaged_directory_stops_uninstall() {
    let rig = Rig::new();
    fs::create_dir_all(&rig.install).unwrap();
    fs::write(rig.install.join("mine.txt"), "keep").unwrap();
    let (code, _, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 1);
    assert!(err.contains("was not created by delega init"), "{err}");
    assert!(rig.install.join("mine.txt").exists());
    rig.no_calls();
}

#[test]
fn a_missing_directory_is_nothing_to_do() {
    let rig = Rig::new();
    let (code, doc, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(doc["directory"], "absent");
    rig.no_calls();
}

#[test]
fn uninstall_dry_run_changes_and_runs_nothing() {
    let rig = installed_for_both();
    let calls = (rig.log("claude", true), rig.log("codex", true));
    let (code, doc, err) = rig.run_json(&["uninstall", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(doc["directory"], "would_remove");
    assert_eq!(doc["hosts"][0]["commands"][0]["status"], "would_run");
    assert_eq!((rig.log("claude", true), rig.log("codex", true)), calls);
    assert!(rig.install.join("plugin/delega/bin/delega").exists());
    let out = rig.run(&["uninstall", "--dry-run", "--host", "claude"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        text(&out.stdout).contains("Would keep"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn a_registration_a_host_no_longer_has_is_not_removed_again() {
    let rig = installed_for_both();
    for host in ["claude", "codex"] {
        fs::remove_file(rig.state(host, "plugin")).unwrap();
        fs::remove_file(rig.state(host, "marketplace")).unwrap();
    }
    let (code, doc, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        rig.log("claude", false)
            .iter()
            .all(|c| !c.contains("remove") && !c.contains("uninstall"))
    );
    assert_eq!(doc["hosts"][0]["commands"], serde_json::json!([]));
    assert!(!rig.install.exists());
}

#[test]
fn a_recorded_host_that_cannot_be_found_keeps_everything() {
    let mut rig = installed_for_both();
    rig.codex = false;
    let before = rig.log("claude", true).len();
    let (code, doc, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("Codex was registered by init but its CLI was not found"),
        "{err}"
    );
    assert_eq!(
        rig.log("claude", true).len(),
        before,
        "a host was called although a CLI is missing"
    );
    assert_eq!(doc["hosts"][0]["host"], "codex");
    assert_eq!(doc["hosts"][0]["status"], "failed");
    assert!(rig.install.exists());
}

// ---- links, scope, stale files, hand-edited markers, JSON failures ----

use std::os::unix::fs::symlink;

fn host_calls(rig: &Rig) -> (Vec<String>, Vec<String>) {
    (rig.log("claude", true), rig.log("codex", true))
}

#[test]
fn a_linked_file_or_directory_in_the_managed_directory_is_refused() {
    for linked_directory in [false, true] {
        let rig = installed_for_both();
        let outside = rig.home.join("outside");
        fs::create_dir_all(outside.join("run")).unwrap();
        let target = outside.join("run/SKILL.md");
        fs::write(&target, "outside content").unwrap();
        let skills = rig.install.join("plugin/delega/skills");
        if linked_directory {
            fs::remove_dir_all(&skills).unwrap();
            symlink(&outside, &skills).unwrap();
        } else {
            let file = skills.join("run/SKILL.md");
            fs::remove_file(&file).unwrap();
            symlink(&target, &file).unwrap();
        }
        let calls = host_calls(&rig);
        for args in [&["init"][..], &["uninstall"]] {
            let (code, _, err) = rig.run_json(args);
            assert_eq!(code, 1, "{args:?} linked_directory={linked_directory}");
            assert!(err.contains("is a symbolic link"), "{args:?}: {err}");
            assert_eq!(fs::read_to_string(&target).unwrap(), "outside content");
            assert_eq!(host_calls(&rig), calls, "{args:?}: a host was called");
            assert!(rig.install.join(".delega-install.json").exists());
        }
        assert!(outside.join("run").is_dir());
    }
}

#[test]
fn claude_registrations_outside_user_scope_are_not_removed() {
    for (variable, expected) in [
        ("FAKE_MARKETPLACE_SCOPE", "another or unverified directory"),
        ("FAKE_PLUGIN_SCOPE", "cannot be attributed to"),
    ] {
        let mut rig = installed_for_both();
        rig.set(variable, "project");
        let (code, _, err) = rig.run_json(&["uninstall", "--host", "claude"]);
        assert_eq!(code, 1, "{variable}");
        assert!(err.contains(expected), "{variable}: {err}");
        assert!(rig.state("claude", "plugin").exists(), "{variable}");
        assert!(rig.state("claude", "marketplace").exists(), "{variable}");
        assert!(
            rig.log("claude", false)
                .iter()
                .all(|c| !c.contains("uninstall") && !c.contains("remove")),
            "{variable}"
        );
        assert!(rig.install.join("plugin/delega/bin/delega").exists());
    }
}

#[test]
fn an_upgrade_deletes_files_an_earlier_install_wrote_and_this_one_does_not() {
    let rig = Rig::new();
    assert_eq!(rig.run(&["init", "--host", "codex"]).status.code(), Some(0));
    let stale = "plugin/delega/skills/retired/SKILL.md";
    fs::create_dir_all(rig.install.join("plugin/delega/skills/retired")).unwrap();
    fs::write(rig.install.join(stale), "old").unwrap();
    let mut marker = rig.marker();
    marker["files"].as_array_mut().unwrap().push(stale.into());
    fs::write(
        rig.install.join(".delega-install.json"),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    let (code, _, err) = rig.run_json(&["init", "--host", "codex"]);
    assert_eq!(code, 0, "{err}");
    assert!(!rig.install.join(stale).exists());
    assert!(
        !rig.install.join("plugin/delega/skills/retired").exists(),
        "the emptied directory stays"
    );
    assert!(!rig.marker()["files"].to_string().contains("retired"));
    assert!(
        rig.install
            .join("plugin/delega/skills/run/SKILL.md")
            .exists()
    );
}

#[test]
fn uninstall_removes_only_the_listed_files_and_reports_files_removed() {
    let rig = installed_for_both();
    fs::write(rig.install.join("extra.txt"), "mine").unwrap();
    let (code, doc, err) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(doc["directory"], "files_removed");
    assert!(rig.install.join("extra.txt").exists());
    assert!(!rig.install.join(".delega-install.json").exists());
    assert!(!rig.install.join("plugin").exists());
    assert!(!rig.install.join(".claude-plugin").exists());
    let names: Vec<_> = fs::read_dir(&rig.install)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["extra.txt"]);
}

#[test]
fn uninstall_removes_an_empty_directory_the_user_chose() {
    let rig = Rig::new();
    fs::create_dir_all(&rig.install).unwrap();
    assert_eq!(rig.run(&["init", "--host", "codex"]).status.code(), Some(0));
    let (code, doc, _) = rig.run_json(&["uninstall"]);
    assert_eq!(code, 0);
    assert_eq!(doc["directory"], "removed");
    assert!(!rig.install.exists());
}

#[test]
fn a_marker_naming_a_path_this_delega_does_not_write_is_refused() {
    for path in [
        "notes.txt",
        "plugin/delega/notes.txt",
        "plugin/delega/skills/run/other.md",
        "plugin/delega/skills/a b/SKILL.md",
        "plugin/other/plugin.json",
    ] {
        let rig = installed_for_both();
        fs::write(rig.install.join("notes.txt"), "mine").unwrap();
        let mut marker = rig.marker();
        marker["files"].as_array_mut().unwrap().push(path.into());
        fs::write(
            rig.install.join(".delega-install.json"),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        let calls = host_calls(&rig);
        for args in [&["uninstall"][..], &["init"]] {
            let (code, _, err) = rig.run_json(args);
            assert_eq!(code, 1, "{path} {args:?}");
            assert!(
                err.contains("is not a path this delega writes"),
                "{path}: {err}"
            );
            assert_eq!(host_calls(&rig), calls, "{path}");
        }
        assert_eq!(
            fs::read_to_string(rig.install.join("notes.txt")).unwrap(),
            "mine"
        );
        assert!(
            rig.install.join("plugin/delega/bin/delega").exists(),
            "{path}"
        );
    }
}

#[test]
fn every_failure_prints_the_json_document() {
    let rig = Rig::new();
    for command in ["init", "uninstall"] {
        let (code, doc, err) = rig.run_json(&[command, "--install-dir", "/"]);
        assert_eq!(code, 1, "{command}");
        assert_eq!(doc["ok"], false);
        assert!(
            doc["error"].as_str().unwrap().contains("filesystem root"),
            "{doc}"
        );
        assert!(err.contains("filesystem root"), "{err}");
    }
    rig.no_calls();
}

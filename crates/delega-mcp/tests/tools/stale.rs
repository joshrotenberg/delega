//! A server whose executable was replaced or removed after it started says so
//! in `list` and in `delega://identity`.
//!
//! The executable is a file in a temporary directory laid out as the installed
//! plugin is (`plugin/bin/delega` with `plugin/IDENTITY.json` beside `bin`),
//! never the test binary. The supervisors start nothing.

use std::path::{Path, PathBuf};

use delega_core::contract::partial::PartialRequest;
use delega_core::provider::ProviderOptions;
use delega_core::supervisor::{Supervisor, SupervisorConfig};
use delega_mcp::{BuildInfo, Executable, IDENTITY_URI, WHAT_TO_DO};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower_mcp::TestClient;

const RUNNING: &str = "running-revision";

/// A plugin directory with an executable, and a server that recorded it.
struct Installed {
    _tmp: TempDir,
    plugin: PathBuf,
    executable: PathBuf,
    supervisor: Supervisor,
    client: TestClient,
}

impl Installed {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().expect("create temporary directory");
        let plugin = tmp.path().join("plugin");
        std::fs::create_dir_all(plugin.join("bin")).expect("create bin");
        let executable = plugin.join("bin/delega");
        std::fs::write(&executable, b"the build that is running").expect("write executable");
        let recorded = Executable::at(&executable).expect("record the executable");
        assert_eq!(recorded.path(), executable);
        let (supervisor, client) = server(Some(recorded)).await;
        Self {
            _tmp: tmp,
            plugin,
            executable,
            supervisor,
            client,
        }
    }

    /// Replace the executable as `delega plugin install` does: a staged file
    /// renamed over the path.
    fn replace_by_rename(&self) {
        let staged = self.plugin.join("bin/delega.staged");
        std::fs::write(&staged, b"the build that was installed").expect("write staged");
        std::fs::rename(&staged, &self.executable).expect("rename over the executable");
    }

    fn write_identity(&self, text: &str) {
        std::fs::write(self.plugin.join("IDENTITY.json"), text).expect("write IDENTITY.json");
    }

    async fn list(&mut self) -> Value {
        list(&mut self.client).await
    }

    async fn identity(&mut self) -> Value {
        identity(&mut self.client).await
    }

    /// `stale_server` of `list`, after checking the identity resource carries
    /// the same object.
    async fn stale_server(&mut self) -> Value {
        let listed = self.list().await;
        let identity = self.identity().await;
        assert_eq!(
            listed.get("stale_server"),
            identity.get("stale_server"),
            "list and the identity resource disagree"
        );
        listed.get("stale_server").cloned().unwrap_or(Value::Null)
    }
}

/// A client over a supervisor that starts nothing, for a server of revision
/// [`RUNNING`] that recorded `executable`.
async fn server(executable: Option<Executable>) -> (Supervisor, TestClient) {
    let supervisor = Supervisor::new(SupervisorConfig {
        provider: ProviderOptions::default(),
        ..SupervisorConfig::default()
    });
    let build = BuildInfo {
        revision: RUNNING.to_string(),
        features: Vec::new(),
    };
    let router = delega_mcp::router_with_executable(
        supervisor.clone(),
        &build,
        PartialRequest::default(),
        None,
        executable,
    )
    .expect("router");
    let mut client = TestClient::from_router(router);
    client.initialize().await;
    (supervisor, client)
}

async fn list(client: &mut TestClient) -> Value {
    let result = client.call_tool("list", json!({})).await;
    assert!(!result.is_error, "{}", result.all_text());
    crate::agree(&result)
}

async fn identity(client: &mut TestClient) -> Value {
    let read = client.read_resource(IDENTITY_URI).await;
    serde_json::from_str(read.contents[0].text.as_deref().expect("text")).expect("JSON")
}

/// The identity document of a server that is not stale.
fn current_identity() -> Value {
    delega_mcp::identity(&BuildInfo {
        revision: RUNNING.to_string(),
        features: Vec::new(),
    })
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

#[tokio::test]
async fn an_untouched_executable_reports_nothing_in_list_or_the_identity() {
    let mut installed = Installed::new().await;
    // An identity beside it changes nothing while the executable is the same.
    installed.write_identity(r#"{"revision":"other"}"#);
    for _ in 0..2 {
        let listed = installed.list().await;
        assert!(listed.get("stale_server").is_none(), "{listed}");
        assert_eq!(listed["workers"], json!([]));
        assert!(listed["limits"].is_object() && listed["cursor"].is_u64());
        assert_eq!(installed.identity().await, current_identity());
    }
    installed.supervisor.shutdown().await;
}

#[tokio::test]
async fn an_executable_replaced_by_rename_is_reported_as_replaced() {
    let mut installed = Installed::new().await;
    assert_eq!(installed.stale_server().await, Value::Null);
    installed.replace_by_rename();
    let stale = installed.stale_server().await;
    assert_eq!(
        stale,
        json!({
            "change": "replaced",
            "running_revision": RUNNING,
            "what_to_do": WHAT_TO_DO,
        })
    );
    assert!(WHAT_TO_DO.contains("/mcp") && WHAT_TO_DO.contains("new session"));
    // The rest of both documents is what a current server gives.
    let listed = installed.list().await;
    assert!(crate::keys_sorted(&listed), "{listed}");
    assert_eq!(listed["workers"], json!([]));
    assert!(listed["limits"].is_object() && listed["cursor"].is_u64());
    let mut identity = installed.identity().await;
    assert!(crate::keys_sorted(&identity), "{identity}");
    identity
        .as_object_mut()
        .expect("object")
        .remove("stale_server");
    assert_eq!(identity, current_identity());
    installed.supervisor.shutdown().await;
}

#[tokio::test]
async fn an_executable_rewritten_in_place_is_reported_as_replaced() {
    let mut installed = Installed::new().await;
    assert_eq!(installed.stale_server().await, Value::Null);
    // The same inode with other content: the size differs.
    std::fs::write(&installed.executable, b"other content, of another length")
        .expect("rewrite the executable");
    let stale = installed.stale_server().await;
    assert_eq!(stale["change"], "replaced", "{stale}");
    assert_eq!(stale["running_revision"], RUNNING);
    installed.supervisor.shutdown().await;
}

#[tokio::test]
async fn a_removed_executable_is_reported_as_removed() {
    let mut installed = Installed::new().await;
    assert_eq!(installed.stale_server().await, Value::Null);
    std::fs::remove_file(&installed.executable).expect("remove the executable");
    let stale = installed.stale_server().await;
    assert_eq!(stale["change"], "removed", "{stale}");
    assert_eq!(stale["what_to_do"], WHAT_TO_DO);
    installed.supervisor.shutdown().await;
}

#[tokio::test]
async fn a_removed_directory_of_the_executable_is_reported_as_removed() {
    // What a Codex refresh does: the versioned cache directory is replaced.
    let mut installed = Installed::new().await;
    installed.write_identity(r#"{"revision":"gone-with-the-directory"}"#);
    std::fs::remove_dir_all(&installed.plugin).expect("remove the plugin directory");
    assert!(!exists(&installed.plugin));
    let stale = installed.stale_server().await;
    assert_eq!(stale["change"], "removed", "{stale}");
    assert!(stale.get("installed_revision").is_none(), "{stale}");
    installed.supervisor.shutdown().await;
}

#[tokio::test]
async fn the_installed_revision_is_read_from_the_identity_beside_the_executable() {
    let mut installed = Installed::new().await;
    installed.replace_by_rename();
    installed.write_identity(r#"{"name":"delega","revision":"installed-revision"}"#);
    let stale = installed.stale_server().await;
    assert_eq!(stale["installed_revision"], "installed-revision", "{stale}");
    assert_eq!(stale["running_revision"], RUNNING);
    // Read on each call, not remembered: a later install is reported too.
    installed.write_identity(r#"{"revision":"a-later-install"}"#);
    let stale = installed.stale_server().await;
    assert_eq!(stale["installed_revision"], "a-later-install", "{stale}");
    installed.supervisor.shutdown().await;
}

#[tokio::test]
async fn the_installed_revision_is_absent_when_the_identity_is_missing_or_unusable() {
    let mut installed = Installed::new().await;
    installed.replace_by_rename();
    let stale = installed.stale_server().await;
    assert!(
        stale.get("installed_revision").is_none(),
        "missing: {stale}"
    );
    let oversized = format!(r#"{{"revision":"big","pad":"{}"}}"#, "x".repeat(64 * 1024));
    for (what, text) in [
        ("not JSON", "not JSON"),
        ("no revision", r#"{"name":"delega"}"#),
        ("a revision that is no string", r#"{"revision":7}"#),
        ("not an object", r#"["revision"]"#),
        ("over the size limit", oversized.as_str()),
    ] {
        installed.write_identity(text);
        let stale = installed.stale_server().await;
        assert_eq!(stale["change"], "replaced", "{what}: {stale}");
        assert!(stale.get("installed_revision").is_none(), "{what}: {stale}");
    }
    // A directory where the file would be is ignored as well.
    std::fs::remove_file(installed.plugin.join("IDENTITY.json")).expect("remove");
    std::fs::create_dir(installed.plugin.join("IDENTITY.json")).expect("create directory");
    let stale = installed.stale_server().await;
    assert!(
        stale.get("installed_revision").is_none(),
        "directory: {stale}"
    );
    installed.supervisor.shutdown().await;
}

#[tokio::test]
async fn a_server_that_recorded_no_executable_never_reports_stale() {
    // What `delega mcp` builds when `current_exe` gave nothing.
    let (supervisor, mut client) = server(None).await;
    for _ in 0..2 {
        let listed = list(&mut client).await;
        assert!(listed.get("stale_server").is_none(), "{listed}");
        assert_eq!(identity(&mut client).await, current_identity());
    }
    supervisor.shutdown().await;
}

#[tokio::test]
async fn an_executable_that_cannot_be_recorded_gives_a_server_that_never_reports_stale() {
    let tmp = tempfile::tempdir().expect("create temporary directory");
    let recorded = Executable::at(tmp.path().join("bin/absent"));
    assert_eq!(recorded, None);
    let (supervisor, mut client) = server(recorded).await;
    let listed = list(&mut client).await;
    assert!(listed.get("stale_server").is_none(), "{listed}");
    supervisor.shutdown().await;
}

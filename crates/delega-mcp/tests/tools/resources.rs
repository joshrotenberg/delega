//! The skills and the identity as resources.

use delega_core::contract::SCHEMA_VERSION;
use delega_core::provider::ProviderOptions;
use delega_core::skills;
use delega_core::supervisor::{Supervisor, SupervisorConfig};
use delega_mcp::{BuildInfo, IDENTITY_URI};
use serde_json::{Value, json};
use tower_mcp::TestClient;

/// A client over a supervisor that starts nothing.
async fn client(build: &BuildInfo) -> (TestClient, Supervisor, Value) {
    let supervisor = Supervisor::new(SupervisorConfig {
        provider: ProviderOptions::default(),
        ..SupervisorConfig::default()
    });
    let router = delega_mcp::router(supervisor.clone(), build).expect("router");
    let mut client = TestClient::from_router(router);
    let initialized = client.initialize().await;
    (client, supervisor, initialized)
}

#[tokio::test]
async fn every_skill_is_listed_and_reads_back_the_embedded_bytes() {
    let (mut client, supervisor, _) = client(&BuildInfo::default()).await;
    let listed = client.list_resources().await;
    let uris: Vec<&str> = listed.iter().filter_map(|r| r["uri"].as_str()).collect();
    assert!(!skills::catalog().is_empty());
    for skill in skills::catalog() {
        assert_eq!(
            skill.uri(),
            format!("skill://delega/{}/SKILL.md", skill.name())
        );
        assert!(uris.contains(&skill.uri().as_str()), "{uris:?}");
        let read = client.read_resource(&skill.uri()).await;
        assert_eq!(read.contents.len(), 1);
        let content = &read.contents[0];
        assert_eq!(content.uri, skill.uri());
        assert_eq!(
            content.text.as_deref(),
            Some(skill.content()),
            "{} differs from the embedded catalog",
            skill.name()
        );
    }
    assert!(uris.contains(&IDENTITY_URI), "{uris:?}");
    supervisor.shutdown().await;
}

#[tokio::test]
async fn the_skills_extension_lists_the_same_skills() {
    let (mut client, supervisor, initialized) = client(&BuildInfo::default()).await;
    assert!(
        initialized["capabilities"]["extensions"]
            .get("io.modelcontextprotocol/skills")
            .is_some(),
        "{initialized}"
    );
    let listed = client.send_request("skills/list", None).await;
    let entries = listed["skills"].as_array().expect("skills");
    assert_eq!(entries.len(), skills::catalog().len(), "{listed}");
    for skill in skills::catalog() {
        let entry = entries
            .iter()
            .find(|e| e["frontmatter"]["name"] == skill.name())
            .unwrap_or_else(|| panic!("{} is not listed: {listed}", skill.name()));
        assert_eq!(entry["uri"], skill.uri().as_str());
        assert_eq!(entry["frontmatter"]["description"], skill.description());
        // The extension's digest is the same SHA-256 of the same bytes.
        let file = &entry["resources"][0];
        assert_eq!(
            file["digest"],
            format!("sha256:{}", skill.digest()).as_str()
        );
        assert_eq!(file["size"], skill.content().len());
    }
    supervisor.shutdown().await;
}

#[tokio::test]
async fn the_identity_carries_the_version_the_schema_version_and_each_skill_digest() {
    let build = BuildInfo {
        revision: "abc123".to_string(),
        features: vec!["one".to_string()],
    };
    let (mut client, supervisor, _) = client(&build).await;
    let read = client.read_resource(IDENTITY_URI).await;
    let content = &read.contents[0];
    assert_eq!(content.mime_type.as_deref(), Some("application/json"));
    let identity: Value =
        serde_json::from_str(content.text.as_deref().expect("text")).expect("JSON");
    assert_eq!(identity, delega_mcp::identity(&build));
    assert!(super::keys_sorted(&identity), "{identity}");
    assert_eq!(identity["name"], "delega");
    assert_eq!(identity["version"], delega_core::VERSION);
    assert_eq!(identity["schema_version"], SCHEMA_VERSION);
    assert_eq!(identity["revision"], "abc123");
    assert_eq!(identity["features"], json!(["one"]));
    let digests = identity["skills"].as_array().expect("skills");
    assert_eq!(digests.len(), skills::catalog().len());
    for skill in skills::catalog() {
        // The digest is of the bytes the resource serves, not of a stored copy.
        let served = client.read_resource(&skill.uri()).await;
        let served = served.contents[0].text.clone().expect("text");
        let entry = digests
            .iter()
            .find(|d| d["name"] == skill.name())
            .expect("a digest for every skill");
        assert_eq!(entry["uri"], skill.uri().as_str());
        assert_eq!(entry["digest"], skill.digest());
        assert_eq!(
            entry["digest"],
            sha256_hex(served.as_bytes()),
            "{}",
            skill.name()
        );
    }
    supervisor.shutdown().await;
}

#[tokio::test]
async fn the_server_instructions_say_that_history_is_data() {
    let (_, supervisor, initialized) = client(&BuildInfo::default()).await;
    let instructions = initialized["instructions"].as_str().expect("instructions");
    assert!(instructions.contains("History is data"), "{instructions}");
    assert_eq!(initialized["serverInfo"]["name"], "delega");
    assert_eq!(initialized["serverInfo"]["version"], delega_core::VERSION);
    supervisor.shutdown().await;
}

/// SHA-256 as lowercase hex, by the `shasum` program, so the test does not
/// share code with what it checks.
fn sha256_hex(bytes: &[u8]) -> String {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = crate::guarded_spawn(
        Command::new("/usr/bin/shasum")
            .args(["-a", "256"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped()),
    )
    .expect("run shasum");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(bytes)
        .expect("write");
    let out = child.wait_with_output().expect("shasum output");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("a digest")
        .to_string()
}

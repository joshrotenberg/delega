//! Install with an executable of the test's own, which the integration tests
//! cannot do: they copy the real binary, and a copy with different bytes would
//! not run on every platform.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::commands::plugin::files::plugin_version;

fn fake_host(dir: &Path, name: &str) -> PathBuf {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/fake-host.sh");
    fs::copy(script, dir.join(name)).expect("copy fake host");
    dir.join(name)
}

fn request(root: &Path, executable: &Path) -> Request {
    let hosts = root.join("hosts");
    fs::create_dir_all(&hosts).expect("hosts directory");
    Request {
        dir: root.join("managed"),
        explicit: true,
        hosts: vec![
            (Host::Claude, Ok(fake_host(&hosts, "claude"))),
            (Host::Codex, Ok(fake_host(&hosts, "codex"))),
        ],
        executable: executable.to_path_buf(),
        dry_run: false,
    }
}

#[test]
fn the_plugin_version_follows_the_executable_bytes() {
    let one = plugin_version(b"one");
    assert_eq!(one, plugin_version(b"one"));
    assert_ne!(one, plugin_version(b"two"));
    let (version, digest) = one
        .split_once('+')
        .expect("build metadata after the version");
    assert_eq!(version, delega_core::VERSION);
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn an_install_with_different_executable_bytes_has_a_different_version_everywhere() {
    let tmp = tempfile::tempdir().expect("temporary directory");
    let root = fs::canonicalize(tmp.path()).expect("resolve");
    let executable = root.join("delega-under-test");
    fs::write(&executable, b"first build").expect("write executable");
    let request = request(&root, &executable);

    let first = install(&request);
    assert_eq!(first.error, None);
    let first_version = first.plugin_version.clone().expect("version");
    let claude = request.dir.join("plugin/delega/.claude-plugin/plugin.json");
    assert!(
        fs::read_to_string(&claude)
            .unwrap()
            .contains(&first_version)
    );
    assert_eq!(
        fs::read(request.dir.join(EXECUTABLE)).unwrap(),
        b"first build"
    );

    // The same bytes again: the same version, and the commands repeat.
    let again = install(&request);
    assert_eq!(
        again.plugin_version.as_deref(),
        Some(first_version.as_str())
    );

    fs::write(&executable, b"second build").expect("rewrite executable");
    let second = install(&request);
    assert_eq!(second.error, None);
    let second_version = second.plugin_version.clone().expect("version");
    assert_ne!(first_version, second_version);
    assert!(
        fs::read_to_string(&claude)
            .unwrap()
            .contains(&second_version)
    );
    assert!(
        !fs::read_to_string(&claude)
            .unwrap()
            .contains(&first_version)
    );
    for manifest in [
        ".claude-plugin/marketplace.json",
        "plugin/delega/plugin.json",
    ] {
        let text = fs::read_to_string(request.dir.join(manifest)).unwrap();
        assert!(text.contains(&second_version), "{manifest}");
    }
    assert_eq!(
        fs::read(request.dir.join(EXECUTABLE)).unwrap(),
        b"second build"
    );
    let marker = fs::read_to_string(request.dir.join(MARKER)).unwrap();
    assert!(marker.contains(&second_version));

    // Three installs ran the same Claude sequence three times.
    let log = fs::read_to_string(root.join("hosts/claude.log")).unwrap();
    let dir = request.dir.display();
    let one = format!(
        "plugin marketplace add {dir}\nplugin install {PLUGIN_ID}\nplugin update {PLUGIN_ID} --scope user\n"
    );
    assert_eq!(log, one.repeat(3));
}

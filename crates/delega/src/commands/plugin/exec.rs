//! Running a host CLI: from a neutral working directory, with no input, a
//! time limit and a bound on the output kept.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How long a host command that changes registrations may run.
pub const CHANGE_LIMIT: Duration = Duration::from_secs(300);
/// How long a host listing may run.
pub const LISTING_LIMIT: Duration = Duration::from_secs(30);
/// The most output kept from each stream of a change command.
pub const CHANGE_BYTES: usize = 1024 * 1024;
/// The most output kept from each stream of a listing.
pub const LISTING_BYTES: usize = 512 * 1024;

/// How long to wait for the output streams to close after the command exited.
const STREAM_GRACE: Duration = Duration::from_secs(5);

/// A host command that ran to an exit.
#[derive(Debug)]
pub struct Ran {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    /// Both streams, trimmed, for showing to the user.
    pub fn shown(&self) -> String {
        let mut text = self.stdout.trim().to_string();
        let stderr = self.stderr.trim();
        if !stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(stderr);
        }
        text
    }
}

fn drain<R: Read + Send + 'static>(
    mut stream: R,
    max: usize,
    which: bool,
    tx: mpsc::Sender<(bool, Vec<u8>)>,
) {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0u8; 8192];
        // Keep reading past the bound so the child never blocks on a full pipe.
        while let Ok(n) = stream.read(&mut buffer) {
            if n == 0 {
                break;
            }
            let room = max.saturating_sub(kept.len());
            kept.extend_from_slice(&buffer[..n.min(room)]);
        }
        let _ = tx.send((which, kept));
    });
}

/// Run `program` with `args` in `/` and wait for it. An error is a command
/// that could not start, timed out (it is killed) or left its output open.
pub fn run(program: &Path, args: &[String], limit: Duration, max: usize) -> Result<Ran, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir("/")
        .env("PWD", "/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Under the process layer's spawn lock, like every process delega starts,
    // so a worker started at the same moment in the same program cannot
    // inherit these pipes, nor this command a worker's (#104).
    // The workspace `clippy.toml` rejects a direct spawn; this one holds the
    // guard (#107).
    #[allow(clippy::disallowed_methods)]
    let mut child = {
        let _held = delega_core::process::spawn_guard();
        command.spawn()
    }
    .map_err(|err| format!("cannot start {}: {err}", program.display()))?;
    let (tx, rx) = mpsc::channel();
    if let Some(out) = child.stdout.take() {
        drain(out, max, false, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        drain(err, max, true, tx.clone());
    }
    drop(tx);
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("cannot wait for {}: {err}", program.display()));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{} did not finish within {} s and was stopped",
                program.display(),
                limit.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for _ in 0..2 {
        match rx.recv_timeout(STREAM_GRACE) {
            Ok((false, bytes)) => stdout = bytes,
            Ok((true, bytes)) => stderr = bytes,
            Err(_) => {
                return Err(format!(
                    "{} exited but its output stayed open",
                    program.display()
                ));
            }
        }
    }
    Ok(Ran {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

//! What `init` and `uninstall` report: one record per host command and per
//! host, as text or as JSON.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::Host;

/// How much of a host's output a report keeps per command.
const OUTPUT_CHARS: usize = 4000;

/// One host command and what became of it.
#[derive(Debug, Clone)]
pub struct CommandRecord {
    pub args: Vec<String>,
    /// `ran`, `would_run`, `failed`, `already_absent` or `not_run`.
    pub status: &'static str,
    pub output: String,
}

impl CommandRecord {
    pub fn new(args: Vec<String>, status: &'static str, output: &str) -> Self {
        let output: String = output.trim().chars().take(OUTPUT_CHARS).collect();
        Self {
            args,
            status,
            output,
        }
    }

    fn json(&self) -> Value {
        json!({"args": self.args, "status": self.status, "output": self.output})
    }
}

/// One host and what was done, or not done, for it.
#[derive(Debug, Clone)]
pub struct HostRecord {
    pub host: Host,
    pub program: Option<PathBuf>,
    pub status: &'static str,
    pub note: Option<String>,
    pub commands: Vec<CommandRecord>,
    pub reload: Option<&'static str>,
}

impl HostRecord {
    pub fn new(host: Host, status: &'static str) -> Self {
        Self {
            host,
            program: None,
            status,
            note: None,
            commands: Vec::new(),
            reload: None,
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "host": self.host.name(),
            "program": self.program.as_ref().map(|p| p.display().to_string()),
            "status": self.status,
            "note": self.note,
            "commands": self.commands.iter().map(CommandRecord::json).collect::<Vec<_>>(),
            "reload": self.reload,
        })
    }

    /// The text lines for this host.
    pub fn text(&self, out: &mut String) {
        match &self.program {
            Some(program) => {
                out.push_str(&format!("{}: {}\n", self.host.label(), program.display()))
            }
            None => out.push_str(&format!("{}: {}\n", self.host.label(), self.status)),
        }
        if let Some(note) = &self.note {
            out.push_str(&format!("  {note}\n"));
        }
        for command in &self.commands {
            let label = match command.status {
                "ran" => "ran",
                "would_run" => "would run",
                "failed" => "failed",
                "already_absent" => "already absent",
                _ => "not run",
            };
            out.push_str(&format!(
                "  {label}: {} {}\n",
                self.host.name(),
                command.args.join(" ")
            ));
            for line in command.output.lines() {
                out.push_str(&format!("      {line}\n"));
            }
        }
    }
}

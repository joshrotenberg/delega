//! `delega skill list|show NAME`: the skills embedded in this executable.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use delega_core::canonical::sorted;
use delega_core::contract::SCHEMA_VERSION;
use delega_core::skills::{self, Skill};
use serde_json::{Value, json};

use super::OutputFormat;

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// List the skills built into this executable
    List {
        /// Print as text or as a versioned JSON catalog
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
    },
    /// Print one skill
    Show {
        /// The skill name
        name: String,
        /// Print as text or as JSON
        #[arg(long, value_enum, default_value = "text")]
        output: OutputFormat,
    },
}

/// The catalog fields shared by the list and show JSON.
fn summary(skill: &Skill) -> Value {
    json!({
        "name": skill.name(),
        "description": skill.description(),
        "uri": skill.uri(),
        "digest": skill.digest(),
    })
}

pub fn run(args: Args) -> ExitCode {
    match args.action {
        Action::List { output } => list(output),
        Action::Show { name, output } => show(&name, output),
    }
}

fn list(output: OutputFormat) -> ExitCode {
    let catalog = skills::catalog();
    let text = match output {
        OutputFormat::Text => catalog
            .iter()
            .map(|skill| format!("{}  {}\n", skill.name(), skill.description()))
            .collect(),
        OutputFormat::Json => format!(
            "{}\n",
            sorted(json!({
                "schema_version": SCHEMA_VERSION,
                "skills": catalog.iter().map(summary).collect::<Vec<_>>(),
            }))
        ),
    };
    super::output::emit("skill list", text.as_bytes())
}

fn show(name: &str, output: OutputFormat) -> ExitCode {
    let Some(skill) = skills::find(name) else {
        let known: Vec<&str> = skills::catalog().iter().map(Skill::name).collect();
        eprintln!(
            "delega skill show: no skill named {name:?}; known skills: {}",
            known.join(", ")
        );
        return ExitCode::from(super::EXIT_REFUSED);
    };
    match output {
        // The exact bytes, with no newline added.
        OutputFormat::Text => super::output::emit("skill show", skill.content().as_bytes()),
        OutputFormat::Json => {
            let mut value = summary(skill);
            value["content"] = Value::String(skill.content().to_string());
            super::output::emit("skill show", format!("{}\n", sorted(value)).as_bytes())
        }
    }
}

//! Checks on a request that do not need the filesystem or a provider.
//!
//! Whether the working directory exists, and whether `native_args` pass the
//! allowlist, are decided where the run starts, not here.

use std::fmt;

use super::SCHEMA_VERSION;
use super::limits::*;
use super::request::{Request, Session};

/// One problem found in a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The field the problem is about, such as `permissions.commands[2]`.
    pub field: String,
    /// What is wrong.
    pub message: String,
}

impl Problem {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl Request {
    /// Check the request and return every problem found, not only the first.
    ///
    /// A request whose `schema_version` is not [`SCHEMA_VERSION`] is rejected:
    /// the version is a single integer that changes only for a break, so any
    /// other value is an unknown major version.
    pub fn validate(&self) -> Result<(), Vec<Problem>> {
        let mut problems = Vec::new();

        if self.schema_version != SCHEMA_VERSION {
            problems.push(Problem::new(
                "schema_version",
                format!(
                    "unsupported version {}; this build supports {}",
                    self.schema_version, SCHEMA_VERSION
                ),
            ));
        }

        if self.prompt.trim().is_empty() {
            problems.push(Problem::new("prompt", "must not be empty"));
        } else if self.prompt.len() > MAX_PROMPT_BYTES {
            problems.push(too_long("prompt", self.prompt.len(), MAX_PROMPT_BYTES));
        }

        if let Some(cwd) = &self.cwd
            && cwd.as_os_str().is_empty()
        {
            problems.push(Problem::new("cwd", "must not be empty"));
        }

        if let Some(name) = &self.name {
            let chars = name.chars().count();
            if name.trim().is_empty() {
                problems.push(Problem::new("name", "must not be empty"));
            } else if chars > MAX_NAME_CHARS {
                problems.push(too_long("name", chars, MAX_NAME_CHARS));
            } else if name.chars().any(char::is_control) {
                problems.push(Problem::new("name", "must not contain control characters"));
            }
        }

        if let Some(instructions) = &self.instructions
            && instructions.len() > MAX_INSTRUCTIONS_BYTES
        {
            problems.push(too_long(
                "instructions",
                instructions.len(),
                MAX_INSTRUCTIONS_BYTES,
            ));
        }

        if let Some(model) = &self.model {
            check_identifier(&mut problems, "model", model);
        }

        if !(MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(&self.timeout) {
            problems.push(Problem::new(
                "timeout",
                format!(
                    "{} seconds is outside the allowed range of {} to {} seconds",
                    self.timeout, MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS
                ),
            ));
        }

        self.validate_permissions(&mut problems);

        if let Some(schema) = &self.answer_schema
            && !(schema.is_object() || schema.is_boolean())
        {
            problems.push(Problem::new(
                "answer_schema",
                "must be a JSON Schema: an object or a boolean",
            ));
        }

        validate_session(&mut problems, &self.session);

        if self.native_args.len() > MAX_NATIVE_ARGS {
            problems.push(too_many(
                "native_args",
                self.native_args.len(),
                MAX_NATIVE_ARGS,
            ));
        }
        for (i, arg) in self.native_args.iter().enumerate() {
            let field = format!("native_args[{i}]");
            if arg.is_empty() {
                problems.push(Problem::new(field, "must not be empty"));
            } else if arg.len() > MAX_NATIVE_ARG_BYTES {
                problems.push(too_long(&field, arg.len(), MAX_NATIVE_ARG_BYTES));
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    fn validate_permissions(&self, problems: &mut Vec<Problem>) {
        let perms = &self.permissions;

        if perms.commands.len() > MAX_COMMANDS {
            problems.push(too_many(
                "permissions.commands",
                perms.commands.len(),
                MAX_COMMANDS,
            ));
        }
        for (i, command) in perms.commands.iter().enumerate() {
            let field = format!("permissions.commands[{i}]");
            if command.trim().is_empty() {
                problems.push(Problem::new(field, "must not be empty"));
            } else if command.len() > MAX_COMMAND_BYTES {
                problems.push(too_long(&field, command.len(), MAX_COMMAND_BYTES));
            }
        }

        if perms.directories.len() > MAX_DIRECTORIES {
            problems.push(too_many(
                "permissions.directories",
                perms.directories.len(),
                MAX_DIRECTORIES,
            ));
        }
        for (i, dir) in perms.directories.iter().enumerate() {
            if dir.as_os_str().is_empty() {
                problems.push(Problem::new(
                    format!("permissions.directories[{i}]"),
                    "must not be empty",
                ));
            }
        }
    }
}

fn validate_session(problems: &mut Vec<Problem>, session: &Session) {
    if let Some(id) = &session.resume {
        check_identifier(problems, "session.resume", id);
        if session.ephemeral {
            problems.push(Problem::new(
                "session",
                "ephemeral and resume cannot both be set",
            ));
        }
    }
}

fn check_identifier(problems: &mut Vec<Problem>, field: &str, value: &str) {
    if value.trim().is_empty() {
        problems.push(Problem::new(field, "must not be empty"));
    } else if value.len() > MAX_IDENTIFIER_BYTES {
        problems.push(too_long(field, value.len(), MAX_IDENTIFIER_BYTES));
    }
}

fn too_long(field: &str, actual: usize, max: usize) -> Problem {
    Problem::new(field, format!("length {actual} exceeds the limit of {max}"))
}

fn too_many(field: &str, actual: usize, max: usize) -> Problem {
    Problem::new(field, format!("{actual} entries exceed the limit of {max}"))
}

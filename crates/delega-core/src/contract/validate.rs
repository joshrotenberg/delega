//! Checks on a request or a result that need neither the filesystem nor a
//! provider.
//!
//! Whether the working directory exists, and whether `native_args` pass the
//! allowlist, are decided where the run starts, not here.

use std::fmt;
use std::path::Path;

use serde::Serialize;

use super::SCHEMA_VERSION;
use super::limits::*;
use super::outcome::Outcome;
use super::permissions::Mode;
use super::request::{Request, Session};
use super::result::RunResult;

/// One problem found in a request or a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// The field the problem is about, such as `permissions.commands[2]`.
    pub field: String,
    /// What is wrong.
    pub message: String,
}

impl Problem {
    pub(super) fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
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

fn finish(problems: Vec<Problem>) -> Result<(), Vec<Problem>> {
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn check_version(problems: &mut Vec<Problem>, version: u32) {
    if version != SCHEMA_VERSION {
        problems.push(Problem::new(
            "schema_version",
            format!("unsupported version {version}; this build supports {SCHEMA_VERSION}"),
        ));
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

        check_version(&mut problems, self.schema_version);

        if self.prompt.trim().is_empty() {
            problems.push(Problem::new("prompt", "must not be empty"));
        } else if self.prompt.len() > MAX_PROMPT_BYTES {
            problems.push(too_long("prompt", self.prompt.len(), MAX_PROMPT_BYTES));
        }
        check_nul(&mut problems, "prompt", &self.prompt);

        if let Some(cwd) = &self.cwd {
            if cwd.as_os_str().is_empty() {
                problems.push(Problem::new("cwd", "must not be empty"));
            }
            check_path(&mut problems, "cwd", cwd);
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
            check_nul(&mut problems, "name", name);
            check_not_option_like(&mut problems, "name", name);
        }

        if let Some(instructions) = &self.instructions {
            if instructions.len() > MAX_INSTRUCTIONS_BYTES {
                problems.push(too_long(
                    "instructions",
                    instructions.len(),
                    MAX_INSTRUCTIONS_BYTES,
                ));
            }
            check_nul(&mut problems, "instructions", instructions);
        }

        if let Some(model) = &self.model {
            check_identifier(&mut problems, "model", model);
        }

        if !(MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(&self.timeout_secs) {
            problems.push(Problem::new(
                "timeout_secs",
                format!(
                    "{} is outside the allowed range of {} to {} seconds",
                    self.timeout_secs, MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS
                ),
            ));
        }

        self.validate_permissions(&mut problems);

        if let Some(schema) = &self.answer_schema {
            if !(schema.is_object() || schema.is_boolean()) {
                problems.push(Problem::new(
                    "answer_schema",
                    "must be a JSON Schema: an object or a boolean",
                ));
            }
            let size = schema.to_string().len();
            if size > MAX_ANSWER_SCHEMA_BYTES {
                problems.push(too_long("answer_schema", size, MAX_ANSWER_SCHEMA_BYTES));
            }
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
                problems.push(Problem::new(&field, "must not be empty"));
            } else if arg.len() > MAX_NATIVE_ARG_BYTES {
                problems.push(too_long(&field, arg.len(), MAX_NATIVE_ARG_BYTES));
            }
            check_nul(&mut problems, &field, arg);
        }

        finish(problems)
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
                problems.push(Problem::new(&field, "must not be empty"));
            } else if command.len() > MAX_COMMAND_BYTES {
                problems.push(too_long(&field, command.len(), MAX_COMMAND_BYTES));
            }
            check_nul(problems, &field, command);
        }
        if perms.mode == Mode::Build && !perms.commands.is_empty() {
            problems.push(Problem::new(
                "permissions.commands",
                "must be empty in build mode, which already runs every command inside the \
                 provider's sandbox",
            ));
        }

        if perms.directories.len() > MAX_DIRECTORIES {
            problems.push(too_many(
                "permissions.directories",
                perms.directories.len(),
                MAX_DIRECTORIES,
            ));
        }
        for (i, dir) in perms.directories.iter().enumerate() {
            let field = format!("permissions.directories[{i}]");
            if dir.as_os_str().is_empty() {
                problems.push(Problem::new(&field, "must not be empty"));
            }
            check_path(problems, &field, dir);
        }
    }
}

impl RunResult {
    /// Check that the result does not contradict itself and return every
    /// problem found.
    ///
    /// Every constructor on [`RunResult`] produces a result that passes. This
    /// catches a result edited by hand or read from elsewhere. Cleanup needs no
    /// check here: [`super::result::Cleanup`] cannot hold leftovers when
    /// confirmed, and reading such JSON fails.
    pub fn validate(&self) -> Result<(), Vec<Problem>> {
        let mut problems = Vec::new();

        check_version(&mut problems, self.schema_version);

        match (self.outcome, &self.error) {
            (Outcome::Completed, Some(_)) => {
                problems.push(Problem::new("error", "must be absent when completed"));
            }
            (Outcome::Completed, None) => {}
            (_, None) => problems.push(Problem::new(
                "error",
                "is required when the outcome is not completed",
            )),
            (_, Some(_)) => {}
        }

        if self.structured_valid.is_some() && self.structured.is_none() {
            problems.push(Problem::new(
                "structured_valid",
                "is only allowed together with structured",
            ));
        }

        if self.outcome != Outcome::Refused && self.provider.is_none() {
            problems.push(Problem::new(
                "provider",
                "is required unless the outcome is refused",
            ));
        }

        finish(problems)
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

/// A value that may become a provider argument: not empty, bounded, free of
/// NUL, and not starting with `-` so it cannot be read as an option.
fn check_identifier(problems: &mut Vec<Problem>, field: &str, value: &str) {
    if value.trim().is_empty() {
        problems.push(Problem::new(field, "must not be empty"));
    } else if value.len() > MAX_IDENTIFIER_BYTES {
        problems.push(too_long(field, value.len(), MAX_IDENTIFIER_BYTES));
    }
    check_nul(problems, field, value);
    check_not_option_like(problems, field, value);
}

fn check_not_option_like(problems: &mut Vec<Problem>, field: &str, value: &str) {
    if value.starts_with('-') {
        problems.push(Problem::new(
            field,
            "must not start with '-': it would be read as a provider option",
        ));
    }
}

fn check_nul(problems: &mut Vec<Problem>, field: &str, value: &str) {
    if value.contains('\0') {
        problems.push(Problem::new(field, "must not contain NUL bytes"));
    }
}

fn check_path(problems: &mut Vec<Problem>, field: &str, path: &Path) {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.first() == Some(&b'-') {
        problems.push(Problem::new(
            field,
            "must not start with '-': it would be read as a provider option",
        ));
    }
    if bytes.len() > MAX_PATH_BYTES {
        problems.push(too_long(field, bytes.len(), MAX_PATH_BYTES));
    }
    if bytes.contains(&0) {
        problems.push(Problem::new(field, "must not contain NUL bytes"));
    }
}

fn too_long(field: &str, actual: usize, max: usize) -> Problem {
    Problem::new(field, format!("length {actual} exceeds the limit of {max}"))
}

fn too_many(field: &str, actual: usize, max: usize) -> Problem {
    Problem::new(field, format!("{actual} entries exceed the limit of {max}"))
}

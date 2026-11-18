//! A request with every field optional: the profile.
//!
//! `docs/api.md` calls a partial request saved as a file a profile and does not
//! name a separate profile schema, so none is generated. A profile file is
//! validated by reading it as a [`PartialRequest`], which rejects unknown
//! fields at every level, and then by [`PartialRequest::into_request`] once it
//! has been merged with explicit values.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::SCHEMA_VERSION;
use super::permissions::{Mode, OnRequest, Permissions};
use super::request::{Effort, Provider, Request, Session};
use super::validate::Problem;

/// The permissions part of a [`PartialRequest`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialPermissions {
    /// See [`Permissions::mode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    /// See [`Permissions::commands`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<String>>,
    /// See [`Permissions::directories`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directories: Option<Vec<PathBuf>>,
    /// See [`Permissions::on_request`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_request: Option<OnRequest>,
}

/// The session part of a [`PartialRequest`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialSession {
    /// See [`Session::ephemeral`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
    /// See [`Session::resume`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
}

/// A request with every field optional, for profiles and for building a request
/// from several sources.
///
/// Deserializing accepts a missing `schema_version`, because explicit values
/// built in code carry none. Use [`PartialRequest::from_profile_json`] for a
/// file, which requires it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialRequest {
    /// See [`Request::schema_version`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    /// See [`Request::provider`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    /// See [`Request::prompt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// See [`Request::cwd`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// See [`Request::name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// See [`Request::instructions`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// See [`Request::model`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// See [`Request::effort`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// See [`Request::timeout_secs`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// See [`Request::permissions`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PartialPermissions>,
    /// See [`Request::answer_schema`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_schema: Option<Value>,
    /// See [`Request::session`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<PartialSession>,
    /// See [`Request::native_args`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_args: Option<Vec<String>>,
}

fn pick<T>(over: Option<T>, base: Option<T>) -> Option<T> {
    over.or(base)
}

impl PartialRequest {
    /// Read a profile file. Unknown fields are errors at every level and
    /// `schema_version` is required.
    pub fn from_profile_json(text: &str) -> Result<Self, serde_json::Error> {
        let partial: Self = serde_json::from_str(text)?;
        if partial.schema_version.is_none() {
            return Err(serde::de::Error::missing_field("schema_version"));
        }
        Ok(partial)
    }

    /// Combine two partial requests; a value set in `overrides` wins over the
    /// one in `self`. Lists replace, they are not concatenated. Within
    /// `permissions` and `session` the choice is made field by field.
    pub fn merge(self, overrides: PartialRequest) -> PartialRequest {
        let permissions = match (self.permissions, overrides.permissions) {
            (None, None) => None,
            (a, b) => {
                let (a, b) = (a.unwrap_or_default(), b.unwrap_or_default());
                Some(PartialPermissions {
                    mode: pick(b.mode, a.mode),
                    commands: pick(b.commands, a.commands),
                    directories: pick(b.directories, a.directories),
                    on_request: pick(b.on_request, a.on_request),
                })
            }
        };
        let session = match (self.session, overrides.session) {
            (None, None) => None,
            (a, b) => {
                let (a, b) = (a.unwrap_or_default(), b.unwrap_or_default());
                Some(PartialSession {
                    ephemeral: pick(b.ephemeral, a.ephemeral),
                    resume: pick(b.resume, a.resume),
                })
            }
        };
        PartialRequest {
            schema_version: pick(overrides.schema_version, self.schema_version),
            provider: pick(overrides.provider, self.provider),
            prompt: pick(overrides.prompt, self.prompt),
            cwd: pick(overrides.cwd, self.cwd),
            name: pick(overrides.name, self.name),
            instructions: pick(overrides.instructions, self.instructions),
            model: pick(overrides.model, self.model),
            effort: pick(overrides.effort, self.effort),
            timeout_secs: pick(overrides.timeout_secs, self.timeout_secs),
            permissions,
            answer_schema: pick(overrides.answer_schema, self.answer_schema),
            session,
            native_args: pick(overrides.native_args, self.native_args),
        }
    }

    /// Fill defaults and validate. Returns every problem found, including
    /// a missing `provider` or `prompt`, or the finished [`Request`].
    pub fn into_request(self) -> Result<Request, Vec<Problem>> {
        let mut problems = Vec::new();
        if self.provider.is_none() {
            problems.push(Problem::new("provider", "is required"));
        }
        if self.prompt.is_none() {
            problems.push(Problem::new("prompt", "is required"));
        }

        let mut request = Request::new(
            self.provider.unwrap_or(Provider::Claude),
            self.prompt.clone().unwrap_or_default(),
        );
        request.schema_version = self.schema_version.unwrap_or(SCHEMA_VERSION);
        request.cwd = self.cwd;
        request.name = self.name;
        request.instructions = self.instructions;
        request.model = self.model;
        request.effort = self.effort;
        if let Some(t) = self.timeout_secs {
            request.timeout_secs = t;
        }
        if let Some(p) = self.permissions {
            let defaults = Permissions::default();
            request.permissions = Permissions {
                mode: p.mode.unwrap_or(defaults.mode),
                commands: p.commands.unwrap_or(defaults.commands),
                directories: p.directories.unwrap_or(defaults.directories),
                on_request: p.on_request.unwrap_or(defaults.on_request),
            };
        }
        request.answer_schema = self.answer_schema;
        if let Some(s) = self.session {
            request.session = Session {
                ephemeral: s.ephemeral.unwrap_or(false),
                resume: s.resume,
            };
        }
        if let Some(a) = self.native_args {
            request.native_args = a;
        }

        if let Err(found) = request.validate() {
            // A missing prompt is already reported as "is required".
            problems.extend(
                found
                    .into_iter()
                    .filter(|p| !(self.prompt.is_none() && p.field == "prompt")),
            );
        }
        if problems.is_empty() {
            Ok(request)
        } else {
            Err(problems)
        }
    }
}

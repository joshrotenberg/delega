//! The text sent to a provider: the worker's standing instructions and the
//! standard input that carries the assignment.

use crate::contract::request::Request;

use super::limits::ARGUMENT_LIMIT;

/// The rules every delegated worker gets, before the request's own
/// instructions.
const PREAMBLE: [&str; 4] = [
    "You are a delegated CLI worker. Complete only the supplied assignment.",
    "Read and follow applicable repository instructions, including AGENTS.md and CLAUDE.md.",
    "The parent conversation is not available. Ask for missing context in your result.",
    "Report changes, validation, and unresolved blockers. Do not delegate further.",
];

/// The text for the provider's system-instruction channel and the bytes for
/// standard input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Assembled {
    /// For `--append-system-prompt` or `developer_instructions`.
    pub system: String,
    /// Written to standard input. The assignment is its first line or lines;
    /// instructions that did not fit an argument come before it.
    pub stdin: Vec<u8>,
    /// Notes for the result.
    pub notes: Vec<String>,
}

/// The shape asked of the final answer when `answer_schema` is set.
fn schema_block(schema: &serde_json::Value) -> String {
    format!(
        "\n\nAnswer format: reply with one JSON value that satisfies this JSON Schema, and \
         nothing else. Do not wrap it in a code fence and do not add commentary.\nJSON Schema: {schema}\n"
    )
}

/// Build the instruction text and standard input for a request.
///
/// Instructions go in the system channel when that argument stays within
/// [`ARGUMENT_LIMIT`]. Larger ones are put on standard input ahead of the
/// assignment, which is weaker than a system instruction, and a note says so.
pub(super) fn assemble(request: &Request) -> Assembled {
    let base = PREAMBLE.join("\n");
    let mut notes = Vec::new();
    let mut stdin = String::new();
    let system = match request.instructions.as_deref() {
        Some(extra) if base.len() + 1 + extra.len() <= ARGUMENT_LIMIT => {
            format!("{base}\n{extra}")
        }
        Some(extra) => {
            stdin.push_str("Additional instructions for this assignment:\n");
            stdin.push_str(extra);
            stdin.push_str("\n\nAssignment:\n");
            notes.push(format!(
                "instructions are {} bytes, over the {} byte argument limit, so they are sent on \
                 standard input before the assignment and not as a system instruction",
                extra.len(),
                ARGUMENT_LIMIT
            ));
            base
        }
        None => base,
    };
    stdin.push_str(&request.prompt);
    if let Some(schema) = &request.answer_schema {
        stdin.push_str(&schema_block(schema));
    }
    Assembled {
        system,
        stdin: stdin.into_bytes(),
        notes,
    }
}

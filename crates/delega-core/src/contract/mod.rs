//! The request and result contract.
//!
//! One JSON request in, one JSON result out, the same from every surface.
//! `docs/api.md` is the design these types follow.

pub mod limits;
pub mod outcome;
pub mod permissions;
pub mod request;
pub mod result;
pub mod schema;
pub mod validate;

/// The version of the request and result contract.
///
/// It is a single integer that changes only for an incompatible change. A
/// request with any other value is rejected by [`request::Request::validate`].
pub const SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
mod tests;

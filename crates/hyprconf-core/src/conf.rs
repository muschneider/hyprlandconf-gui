// SPDX-License-Identifier: MIT OR Apache-2.0
//! The legacy hyprlang `.conf` format: a round-trip-preserving parser,
//! serializer, and a mapper onto the semantic [`crate::Config`].
//!
//! The design separates two concerns:
//!
//! - [`ConfDocument`] is a **lossless** line model. Parsing keeps every line
//!   verbatim, so re-serializing an unedited document is byte-for-byte
//!   identical, and editing a single option rewrites only that one line.
//! - [`document_to_config`] / [`bundle_to_config`] interpret a document into
//!   the format-agnostic [`crate::Config`] (expanding `$variables`, mapping
//!   keys onto the [`crate::Schema`], parsing directives), reporting
//!   [`ConfWarning`]s for anything unknown — never dropping it.
//!
//! Includes (`source = path`) are followed by [`ConfParser::parse_file`], which
//! resolves relative/absolute/glob paths and reports cycles and missing files
//! as typed [`ConfError`]s.

mod document;
mod mapper;
mod parser;
mod serializer;

pub use document::{
    Assignment, ConfDocument, Directive, Line, LineEnding, LineKind, SetOutcome, Source,
};
pub(crate) use mapper::bool_as_mode;
pub use mapper::{bundle_to_config, document_to_config, ConfWarning};
pub use parser::{ConfBundle, ConfError, ConfParser};
pub use serializer::{config_to_conf, value_to_conf, ConfSerializer};

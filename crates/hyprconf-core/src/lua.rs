// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Hyprland Lua format (the `hl` global API), the default since 0.55 and
//! the **only** supported format from 0.57 onwards.
//!
//! ## Reading strategy
//!
//! The reader is a **lossless static parse** of the declarative subset via the
//! [`full_moon`] crate (a concrete-syntax Lua parser). This is the right choice
//! for an *editor*: comments, ordering and formatting survive round-trips, and
//! we never have to execute untrusted config code to read it.
//!
//! - [`LuaDocument`] owns the parsed `Ast`; `to_text()` reproduces the source
//!   exactly (full_moon is lossless), so unedited files round-trip byte-for-byte.
//! - [`document_to_config`] / [`bundle_to_config`] interpret the recognised
//!   `hl.*` calls (and top-level `require`) into the format-agnostic
//!   [`crate::Config`].
//! - Anything outside the declarative subset (arbitrary functions, loops,
//!   conditionals, `hl.on`/`hl.timer` closures, variable-captured `require`s,
//!   ...) is **not interpreted**: it stays verbatim in the document and is
//!   surfaced as [`LuaWarning::DynamicRegion`] so a GUI can show it read-only
//!   and never silently flatten user logic.
//!
//! A sandboxed `mlua` evaluation path (for full fidelity on dynamic configs) is
//! intentionally not implemented; the static path is what editing is built on.
//!
//! ## Writing strategy
//!
//! [`LuaSerializer::generate`] emits fresh Lua against the real `hl` API. The
//! shapes are not guessed: they were verified against Hyprland 0.56.1 using
//! `Hyprland --verify-config` and cross-checked against the example config
//! Hyprland ships at `/usr/share/hypr/hyprland.lua`. See [`serializer`] for the
//! table of constructs.
//!
//! Two sub-modules exist purely because the `.conf` -> Lua translation is *not*
//! a syntax change but a semantic one:
//!
//! - [`dispatch`] maps hyprlang dispatcher names (`killactive`) onto the
//!   `hl.dsp.*` constructors Hyprland demands (`hl.dsp.window.close()`); a
//!   string dispatcher is rejected outright.
//! - [`rules`] maps rule text (`opacity 0.9 0.9`) onto typed table fields
//!   (`opacity = "0.9 0.9"`); putting rule text in `name` loads without error
//!   and silently does nothing.
//!
//! Anything that cannot be expressed exactly is reported as a [`LuaNote`]
//! rather than being dropped quietly.

pub mod dispatch;
mod document;
mod emit;
mod extract;
mod mapper;
mod parser;
pub(crate) mod rules;
mod serializer;

pub use document::LuaDocument;
pub use mapper::{bundle_to_config, document_to_config, LuaWarning};
pub use parser::{LuaBundle, LuaError, LuaParser};
pub use rules::{layer_rule_effects, window_rule_effects, workspace_rule_keys};
pub use serializer::{config_call, monitor_call, value_to_lua, LuaNote, LuaOutput, LuaSerializer};

// SPDX-License-Identifier: MIT OR Apache-2.0
//! A tiny Lua literal builder used by the serializer.
//!
//! Hyprland's `hl` API is strongly typed on the C++ side: it rejects unknown
//! table fields and wrong value shapes outright (`hl.window_rule: unknown field
//! 'foo'`). Emitting Lua by `format!`-ing strings made that easy to get wrong,
//! so every generated table now goes through [`Table`]/[`Expr`], which keeps the
//! *structure* explicit and centralises quoting, escaping and number formatting.

use crate::lua::extract::escape;

/// A Lua expression that can be rendered to source text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    /// A quoted, escaped string literal.
    Str(String),
    /// A bare numeric literal.
    Num(f64),
    /// An integer literal (kept distinct so `1` never renders as `1.0`).
    Int(i64),
    /// `true` / `false`.
    Bool(bool),
    /// A key/value table: `{ a = 1, b = "x" }`.
    Table(Table),
    /// A positional array: `{ 1, 2 }`.
    Array(Vec<Expr>),
    /// Verbatim Lua source (a call, an identifier, ...). Used for things like
    /// `hl.dsp.window.close()` that are not literals.
    Raw(String),
}

impl Expr {
    /// A string literal.
    pub(crate) fn str(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }

    /// Verbatim Lua source.
    pub(crate) fn raw(s: impl Into<String>) -> Self {
        Self::Raw(s.into())
    }

    /// A number, rendered as an integer when it is integral.
    pub(crate) fn num(v: f64) -> Self {
        if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
            Self::Int(v as i64)
        } else {
            Self::Num(v)
        }
    }

    /// Parse `text` into the most specific literal it can represent.
    ///
    /// Hyprland distinguishes `workspace = 3` from `workspace = "3"` in some
    /// places but accepts either in most; preferring the natural type keeps the
    /// output idiomatic and matches the shipped example config.
    pub(crate) fn infer(text: &str) -> Self {
        let t = text.trim();
        match t {
            "true" => return Self::Bool(true),
            "false" => return Self::Bool(false),
            _ => {}
        }
        if let Ok(i) = t.parse::<i64>() {
            return Self::Int(i);
        }
        if let Ok(f) = t.parse::<f64>() {
            if f.is_finite() {
                return Self::Num(f);
            }
        }
        Self::Str(t.to_string())
    }

    /// Render as single-line Lua source.
    pub(crate) fn render(&self) -> String {
        match self {
            Self::Str(s) => format!("\"{}\"", escape(s)),
            Self::Num(v) => format_float(*v),
            Self::Int(i) => i.to_string(),
            Self::Bool(b) => b.to_string(),
            Self::Raw(s) => s.clone(),
            Self::Table(t) => t.render_inline(),
            Self::Array(items) => {
                let inner = items
                    .iter()
                    .map(Expr::render)
                    .collect::<Vec<_>>()
                    .join(", ");
                if inner.is_empty() {
                    "{}".to_string()
                } else {
                    format!("{{ {inner} }}")
                }
            }
        }
    }
}

/// An ordered key/value Lua table literal.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Table {
    fields: Vec<(String, Expr)>,
}

impl Table {
    /// An empty table.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Append `key = value`.
    pub(crate) fn set(&mut self, key: impl Into<String>, value: Expr) -> &mut Self {
        self.fields.push((key.into(), value));
        self
    }

    /// Append `key = "value"`.
    pub(crate) fn set_str(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> &mut Self {
        self.set(key, Expr::str(value))
    }

    /// Append `key = "value"` only when `value` is non-empty.
    pub(crate) fn set_str_opt(
        &mut self,
        key: impl Into<String>,
        value: impl AsRef<str>,
    ) -> &mut Self {
        let v = value.as_ref();
        if !v.is_empty() {
            self.set_str(key, v);
        }
        self
    }

    /// Whether any field has been set.
    pub(crate) fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// `{ a = 1, b = "x" }` on one line.
    pub(crate) fn render_inline(&self) -> String {
        if self.fields.is_empty() {
            return "{}".to_string();
        }
        let inner = self
            .fields
            .iter()
            .map(|(k, v)| format!("{} = {}", key(k), v.render()))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{{ {inner} }}")
    }

    /// Wrap as an [`Expr`].
    pub(crate) fn into_expr(self) -> Expr {
        Expr::Table(self)
    }
}

/// Render a Lua table key: bare when it is a valid identifier, else `["..."]`.
pub(crate) fn key(k: &str) -> String {
    if is_ident(k) {
        k.to_string()
    } else {
        format!("[\"{}\"]", escape(k))
    }
}

/// Whether `s` is a valid, non-reserved Lua identifier.
pub(crate) fn is_ident(s: &str) -> bool {
    const RESERVED: &[&str] = &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
        "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];
    let mut chars = s.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    first_ok && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !RESERVED.contains(&s)
}

/// Format an `f64` without a trailing `.0` for whole numbers.
pub(crate) fn format_float(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// Render a call `name(arg1, arg2, ...)`, skipping trailing empty arguments.
pub(crate) fn call(name: &str, args: &[Expr]) -> String {
    let inner = args.iter().map(Expr::render).collect::<Vec<_>>().join(", ");
    format!("{name}({inner})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_scalars() {
        assert_eq!(Expr::str("a\"b").render(), "\"a\\\"b\"");
        assert_eq!(Expr::Bool(true).render(), "true");
        assert_eq!(Expr::num(1.0).render(), "1");
        assert_eq!(Expr::num(0.5).render(), "0.5");
        assert_eq!(Expr::Int(-1).render(), "-1");
    }

    #[test]
    fn infer_picks_the_narrowest_literal() {
        assert_eq!(Expr::infer("3"), Expr::Int(3));
        assert_eq!(Expr::infer("-1"), Expr::Int(-1));
        assert_eq!(Expr::infer("0.9"), Expr::Num(0.9));
        assert_eq!(Expr::infer("true"), Expr::Bool(true));
        assert_eq!(Expr::infer("auto"), Expr::Str("auto".into()));
        // `special:magic` must stay a string, not be mangled into a number.
        assert_eq!(
            Expr::infer("special:magic"),
            Expr::Str("special:magic".into())
        );
    }

    #[test]
    fn renders_inline_tables() {
        let mut t = Table::new();
        t.set_str("output", "DP-1").set("scale", Expr::num(1.5));
        assert_eq!(t.render_inline(), "{ output = \"DP-1\", scale = 1.5 }");
        assert_eq!(Table::new().render_inline(), "{}");
    }

    #[test]
    fn quotes_non_identifier_keys() {
        assert_eq!(key("rounding"), "rounding");
        assert_eq!(key("col.active_border"), "[\"col.active_border\"]");
        assert_eq!(key("end"), "[\"end\"]");
    }

    #[test]
    fn set_str_opt_skips_empty_values() {
        let mut t = Table::new();
        t.set_str_opt("mode", "").set_str_opt("position", "auto");
        assert_eq!(t.render_inline(), "{ position = \"auto\" }");
    }

    #[test]
    fn arrays_render_positionally() {
        let e = Expr::Array(vec![Expr::num(0.25), Expr::num(1.0)]);
        assert_eq!(e.render(), "{ 0.25, 1 }");
        assert_eq!(Expr::Array(Vec::new()).render(), "{}");
    }
}

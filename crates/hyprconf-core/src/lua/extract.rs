// SPDX-License-Identifier: MIT OR Apache-2.0
//! Low-level helpers that lower the relevant slice of a `full_moon` AST into a
//! small, full_moon-independent intermediate ([`LuaVal`]).
//!
//! The mapper and the include-follower both work against this intermediate so
//! the gnarly `full_moon` pattern matching lives in exactly one place. Anything
//! outside the declarative subset (functions, operators, variables, method
//! chains, ...) collapses to [`LuaVal::Other`], which the mapper treats as
//! "dynamic" and leaves to the lossless document.

use full_moon::ast::{
    Call, Expression, Field, FunctionArgs, FunctionCall, Index, Prefix, Stmt, Suffix,
    TableConstructor, UnOp,
};
use full_moon::tokenizer::{TokenReference, TokenType};

/// A value lowered from a Lua expression, restricted to the declarative subset.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LuaVal {
    /// A string literal (already un-escaped).
    Str(String),
    /// A numeric literal, kept as its source text.
    Num(String),
    /// `true` / `false`.
    Bool(bool),
    /// `nil`.
    Nil,
    /// A table constructor, fields in source order.
    Table(Vec<LuaField>),
    /// Anything outside the declarative subset.
    Other,
}

/// One field of a table: an optional key plus a value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LuaField {
    /// The field key (`name` or `["name"]`); `None` for a positional element.
    pub key: Option<String>,
    /// The field value.
    pub value: LuaVal,
}

/// Extract the identifier text of a token, if it is an identifier.
fn ident_text(tr: &TokenReference) -> Option<String> {
    match tr.token().token_type() {
        TokenType::Identifier { identifier } => Some(identifier.as_str().to_string()),
        _ => None,
    }
}

/// Lower an [`Expression`] to a [`LuaVal`].
pub(crate) fn expr_to_luaval(expr: &Expression) -> LuaVal {
    match expr {
        Expression::String(tr) => match tr.token().token_type() {
            TokenType::StringLiteral { literal, .. } => LuaVal::Str(unescape(literal.as_str())),
            _ => LuaVal::Other,
        },
        Expression::Number(tr) => match tr.token().token_type() {
            TokenType::Number { text } => LuaVal::Num(text.as_str().to_string()),
            _ => LuaVal::Other,
        },
        Expression::Symbol(tr) => match tr.token().token_type() {
            TokenType::Symbol { symbol } => match symbol.to_string().as_str() {
                "true" => LuaVal::Bool(true),
                "false" => LuaVal::Bool(false),
                "nil" => LuaVal::Nil,
                _ => LuaVal::Other,
            },
            _ => LuaVal::Other,
        },
        Expression::TableConstructor(tc) => table_to_luaval(tc),
        Expression::Parentheses { expression, .. } => expr_to_luaval(expression),
        // Lua has no negative number *literals*: `-1` is unary minus applied to
        // the literal `1`. Fold `-<number>` back into a numeric value so signed
        // integer/float options — and numeric "mode" enums like
        // `force_default_wallpaper = -1` — round-trip. Other unary ops (`not`,
        // `#`) remain dynamic.
        Expression::UnaryOperator {
            unop: UnOp::Minus(_),
            expression,
        } => match expr_to_luaval(expression) {
            LuaVal::Num(n) => LuaVal::Num(format!("-{n}")),
            _ => LuaVal::Other,
        },
        _ => LuaVal::Other,
    }
}

fn table_to_luaval(tc: &TableConstructor) -> LuaVal {
    let mut fields = Vec::new();
    for field in tc.fields() {
        match field {
            Field::NameKey { key, value, .. } => fields.push(LuaField {
                key: ident_text(key),
                value: expr_to_luaval(value),
            }),
            Field::ExpressionKey { key, value, .. } => {
                let key = match expr_to_luaval(key) {
                    LuaVal::Str(s) => Some(s),
                    _ => None,
                };
                fields.push(LuaField {
                    key,
                    value: expr_to_luaval(value),
                });
            }
            Field::NoKey(expr) => fields.push(LuaField {
                key: None,
                value: expr_to_luaval(expr),
            }),
            _ => {}
        }
    }
    LuaVal::Table(fields)
}

/// Lower the arguments of a call into a list of [`LuaVal`]s.
pub(crate) fn args_to_luavals(args: &FunctionArgs) -> Vec<LuaVal> {
    match args {
        FunctionArgs::Parentheses { arguments, .. } => {
            arguments.iter().map(expr_to_luaval).collect()
        }
        FunctionArgs::TableConstructor(tc) => vec![table_to_luaval(tc)],
        FunctionArgs::String(tr) => match tr.token().token_type() {
            TokenType::StringLiteral { literal, .. } => {
                vec![LuaVal::Str(unescape(literal.as_str()))]
            }
            _ => vec![LuaVal::Other],
        },
        _ => Vec::new(),
    }
}

/// If `fc` is a simple `a.b.c(args)` call (no method calls, brackets or chained
/// calls), return the dotted callee name and its argument list.
pub(crate) fn callee_path(fc: &FunctionCall) -> Option<(String, Vec<LuaVal>)> {
    let mut parts = Vec::new();
    match fc.prefix() {
        Prefix::Name(tr) => parts.push(ident_text(tr)?),
        Prefix::Expression(_) => return None,
        _ => return None,
    }

    let mut args = None;
    for suffix in fc.suffixes() {
        match suffix {
            Suffix::Index(Index::Dot { name, .. }) => {
                if args.is_some() {
                    return None;
                }
                parts.push(ident_text(name)?);
            }
            Suffix::Call(Call::AnonymousCall(fa)) => {
                if args.is_some() {
                    return None;
                }
                args = Some(args_to_luavals(fa));
            }
            _ => return None,
        }
    }

    Some((parts.join("."), args?))
}

/// Collect the module strings of every top-level `require("module")` statement,
/// in source order.
pub(crate) fn require_modules(stmts: &[&Stmt]) -> Vec<String> {
    let mut modules = Vec::new();
    for stmt in stmts {
        if let Stmt::FunctionCall(fc) = stmt {
            if let Some((callee, args)) = callee_path(fc) {
                if callee == "require" {
                    if let Some(LuaVal::Str(module)) = args.first() {
                        modules.push(module.clone());
                    }
                }
            }
        }
    }
    modules
}

/// Un-escape the common Lua string escapes we emit/consume.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Escape a string for emission inside a double-quoted Lua literal.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_stmt_call(code: &str) -> FunctionCall {
        let ast = full_moon::parse(code).unwrap();
        let stmt = ast.nodes().stmts().next().unwrap().clone();
        match stmt {
            Stmt::FunctionCall(fc) => fc,
            other => panic!("expected call, got {other:?}"),
        }
    }

    #[test]
    fn extracts_dotted_callee_and_args() {
        let fc = first_stmt_call("hl.env(\"X\", \"1\")\n");
        let (callee, args) = callee_path(&fc).unwrap();
        assert_eq!(callee, "hl.env");
        assert_eq!(args, vec![LuaVal::Str("X".into()), LuaVal::Str("1".into())]);
    }

    #[test]
    fn lowers_table_with_mixed_keys() {
        let fc = first_stmt_call("hl.x({ a = 1, [\"b.c\"] = true, \"pos\" })\n");
        let (_callee, args) = callee_path(&fc).unwrap();
        let LuaVal::Table(fields) = &args[0] else {
            panic!("expected table");
        };
        assert_eq!(fields[0].key.as_deref(), Some("a"));
        assert_eq!(fields[0].value, LuaVal::Num("1".into()));
        assert_eq!(fields[1].key.as_deref(), Some("b.c"));
        assert_eq!(fields[1].value, LuaVal::Bool(true));
        assert_eq!(fields[2].key, None);
    }

    #[test]
    fn negative_numbers_fold_into_numeric_values() {
        // `-1` is unary-minus over `1` in Lua; it must lower to a numeric value
        // (not `Other`) so signed options and numeric "mode" enums round-trip.
        let fc = first_stmt_call("hl.config({ misc = { force_default_wallpaper = -1 } })\n");
        let (_callee, args) = callee_path(&fc).unwrap();
        let LuaVal::Table(outer) = &args[0] else {
            panic!("expected table");
        };
        let LuaVal::Table(misc) = &outer[0].value else {
            panic!("expected nested table");
        };
        assert_eq!(misc[0].key.as_deref(), Some("force_default_wallpaper"));
        assert_eq!(misc[0].value, LuaVal::Num("-1".into()));
    }

    #[test]
    fn method_calls_are_not_simple_callees() {
        let fc = first_stmt_call("hl.bind(\"a\", \"b\"):remove()\n");
        assert!(callee_path(&fc).is_none(), "method chain must be rejected");
    }

    #[test]
    fn require_modules_are_collected() {
        let ast = full_moon::parse("require(\"a\")\nrequire(\"b/c\")\nlocal x = 1\n").unwrap();
        let stmts: Vec<&Stmt> = ast.nodes().stmts().collect();
        assert_eq!(
            require_modules(&stmts),
            vec!["a".to_string(), "b/c".to_string()]
        );
    }

    #[test]
    fn escape_round_trips() {
        let s = "say \"hi\"\n\\done";
        assert_eq!(unescape(&escape(s)), s);
    }
}

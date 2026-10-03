// SPDX-License-Identifier: MIT OR Apache-2.0
//! Scalar configuration values and their Hyprland textual representations.
//!
//! These types are deliberately *format-agnostic*: a [`Color`] knows how to
//! parse from and render to Hyprland's `rgba(...)` / `0x...` syntaxes, but it
//! does not care whether it lives in a `.lua` or `.conf` file. The structured,
//! repeatable constructs (keybinds, rules, ...) live in [`crate::structured`].

use std::fmt;

/// Error returned when parsing a scalar value from its Hyprland textual form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ValueParseError {
    /// A color literal was malformed.
    #[error("invalid color {input:?}: {reason}")]
    Color {
        /// The offending input.
        input: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// A gradient literal was malformed.
    #[error("invalid gradient {input:?}: {reason}")]
    Gradient {
        /// The offending input.
        input: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// A `Vec2` literal was malformed.
    #[error("invalid vec2 {input:?}: {reason}")]
    Vec2 {
        /// The offending input.
        input: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// A CSS-style gap literal was malformed.
    #[error("invalid gap {input:?}: {reason}")]
    CssGap {
        /// The offending input.
        input: String,
        /// Why it was rejected.
        reason: &'static str,
    },
}

/// An 8-bit-per-channel RGBA color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel (`255` = fully opaque).
    pub a: u8,
}

impl Color {
    /// Construct a color from explicit RGBA channels.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Construct an opaque color from RGB channels (alpha = `255`).
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Parse a color from any of Hyprland's accepted textual forms:
    ///
    /// - `rgba(RRGGBBAA)` — 8 hex digits.
    /// - `rgb(RRGGBB)` — 6 hex digits (alpha = `ff`).
    /// - `0xAARRGGBB` — legacy ARGB hex (8 digits); `0xRRGGBB` is also accepted
    ///   as opaque RGB.
    ///
    /// # Errors
    ///
    /// Returns [`ValueParseError::Color`] if `input` is not one of the above.
    pub fn from_hyprland_str(input: &str) -> Result<Self, ValueParseError> {
        let s = input.trim();
        let err = |reason: &'static str| ValueParseError::Color {
            input: input.to_string(),
            reason,
        };

        if let Some(hex) = s.strip_prefix("rgba(").and_then(|x| x.strip_suffix(')')) {
            let [r, g, b, a] =
                hex_bytes::<4>(hex.trim()).ok_or_else(|| err("rgba() expects 8 hex digits"))?;
            return Ok(Self::rgba(r, g, b, a));
        }
        if let Some(hex) = s.strip_prefix("rgb(").and_then(|x| x.strip_suffix(')')) {
            let [r, g, b] =
                hex_bytes::<3>(hex.trim()).ok_or_else(|| err("rgb() expects 6 hex digits"))?;
            return Ok(Self::rgb(r, g, b));
        }
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            if let Some([a, r, g, b]) = hex_bytes::<4>(hex) {
                return Ok(Self::rgba(r, g, b, a));
            }
            if let Some([r, g, b]) = hex_bytes::<3>(hex) {
                return Ok(Self::rgb(r, g, b));
            }
            return Err(err("0x color expects 6 (RGB) or 8 (ARGB) hex digits"));
        }

        Err(err("expected rgba(...), rgb(...), or 0x... form"))
    }

    /// Render the color as `rgba(RRGGBBAA)` — hyprconf's canonical output form.
    #[must_use]
    pub fn to_rgba_string(&self) -> String {
        format!(
            "rgba({:02x}{:02x}{:02x}{:02x})",
            self.r, self.g, self.b, self.a
        )
    }

    /// Build a color from HSV (`h` in degrees, `s`/`v` in `0.0..=1.0`) plus an
    /// explicit alpha. Inputs are wrapped/clamped, so this never fails — handy
    /// for a visual color picker. Round-trips with [`Color::to_hsv`] for any
    /// color a picker can produce.
    #[must_use]
    pub fn from_hsv(h: f64, s: f64, v: f64, a: u8) -> Self {
        let h = h.rem_euclid(360.0);
        let s = s.clamp(0.0, 1.0);
        let v = v.clamp(0.0, 1.0);

        let c = v * s;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let m = v - c;
        let (r1, g1, b1) = match (h / 60.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let to_u8 = |f: f64| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        Self::rgba(to_u8(r1), to_u8(g1), to_u8(b1), a)
    }

    /// Decompose into HSV: hue in degrees (`0.0..360.0`) and `s`/`v` in
    /// `0.0..=1.0`. Alpha is ignored. For greys the hue is reported as `0`.
    #[must_use]
    pub fn to_hsv(&self) -> (f64, f64, f64) {
        let r = f64::from(self.r) / 255.0;
        let g = f64::from(self.g) / 255.0;
        let b = f64::from(self.b) / 255.0;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;

        let hue = if delta == 0.0 {
            0.0
        } else if max == r {
            60.0 * (((g - b) / delta).rem_euclid(6.0))
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };

        let saturation = if max == 0.0 { 0.0 } else { delta / max };
        (hue.rem_euclid(360.0), saturation, max)
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rgba_string())
    }
}

/// A gradient: one or more color stops with an optional angle (in degrees).
///
/// Used for options like `general:col.active_border`, which Hyprland renders as
/// space-separated colors optionally followed by `<angle>deg`.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    /// The ordered color stops; always at least one.
    pub stops: Vec<Color>,
    /// Optional rotation in degrees.
    pub angle_deg: Option<f64>,
}

impl Gradient {
    /// A gradient consisting of a single solid color and no angle.
    #[must_use]
    pub fn solid(color: Color) -> Self {
        Self {
            stops: vec![color],
            angle_deg: None,
        }
    }

    /// A multi-stop gradient with an explicit angle.
    #[must_use]
    pub fn with_angle(stops: Vec<Color>, angle_deg: f64) -> Self {
        Self {
            stops,
            angle_deg: Some(angle_deg),
        }
    }

    /// Parse a gradient: whitespace-separated colors, optionally terminated by a
    /// `<number>deg` angle token (e.g. `rgba(11223344) rgba(aabbccdd) 45deg`).
    ///
    /// # Errors
    ///
    /// Returns [`ValueParseError::Gradient`] if the input is empty, has no
    /// colors, or contains an unparseable color/angle.
    pub fn from_hyprland_str(input: &str) -> Result<Self, ValueParseError> {
        let err = |reason: &'static str| ValueParseError::Gradient {
            input: input.to_string(),
            reason,
        };

        let mut tokens: Vec<&str> = input.split_whitespace().collect();
        if tokens.is_empty() {
            return Err(err("empty gradient"));
        }

        let mut angle_deg = None;
        if let Some(num) = tokens.last().and_then(|t| t.strip_suffix("deg")) {
            angle_deg = Some(num.parse::<f64>().map_err(|_| err("invalid angle"))?);
            tokens.pop();
        }

        if tokens.is_empty() {
            return Err(err("gradient has an angle but no colors"));
        }

        let mut stops = Vec::with_capacity(tokens.len());
        for token in tokens {
            stops.push(Color::from_hyprland_str(token).map_err(|_| err("invalid color stop"))?);
        }

        Ok(Self { stops, angle_deg })
    }

    /// Render the gradient back to Hyprland's textual form.
    #[must_use]
    pub fn to_hyprland_string(&self) -> String {
        let mut out = self
            .stops
            .iter()
            .map(Color::to_rgba_string)
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(angle) = self.angle_deg {
            out.push(' ');
            out.push_str(&format_f64(angle));
            out.push_str("deg");
        }
        out
    }
}

impl fmt::Display for Gradient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hyprland_string())
    }
}

/// A 2D vector, used by options such as `decoration:shadow:offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2 {
    /// X component.
    pub x: f64,
    /// Y component.
    pub y: f64,
}

impl Vec2 {
    /// Construct a [`Vec2`].
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Parse `"x y"` or `"x, y"` (commas are treated as whitespace).
    ///
    /// # Errors
    ///
    /// Returns [`ValueParseError::Vec2`] unless exactly two parseable numbers
    /// are present.
    pub fn from_hyprland_str(input: &str) -> Result<Self, ValueParseError> {
        let err = |reason: &'static str| ValueParseError::Vec2 {
            input: input.to_string(),
            reason,
        };

        let cleaned = input.replace(',', " ");
        let parts: Vec<&str> = cleaned.split_whitespace().collect();
        if parts.len() != 2 {
            return Err(err("expected two numbers"));
        }
        let x = parts[0]
            .parse::<f64>()
            .map_err(|_| err("x is not a number"))?;
        let y = parts[1]
            .parse::<f64>()
            .map_err(|_| err("y is not a number"))?;
        Ok(Self::new(x, y))
    }

    /// Render as `"x y"`.
    #[must_use]
    pub fn to_hyprland_string(&self) -> String {
        format!("{} {}", format_f64(self.x), format_f64(self.y))
    }
}

impl fmt::Display for Vec2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hyprland_string())
    }
}

/// Per-side gaps, as Hyprland's `gaps_in` / `gaps_out` / `float_gaps` accept
/// them: CSS shorthand in `.conf` (`5`, `5 10`, `5 10 15`, `5 10 15 20`) and an
/// integer or a `{ top, right, bottom, left }` table in Lua.
///
/// Modelling these as a plain integer (as earlier versions did) silently
/// rejected every per-side value on load — and Lua refuses the `.conf` string
/// form outright (*"css_gap type requires an integer or a table"*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CssGap {
    /// Top gap in px.
    pub top: i64,
    /// Right gap in px.
    pub right: i64,
    /// Bottom gap in px.
    pub bottom: i64,
    /// Left gap in px.
    pub left: i64,
}

impl CssGap {
    /// The same gap on every side.
    #[must_use]
    pub const fn uniform(px: i64) -> Self {
        Self {
            top: px,
            right: px,
            bottom: px,
            left: px,
        }
    }

    /// Whether every side has the same value.
    #[must_use]
    pub fn is_uniform(&self) -> bool {
        self.top == self.right && self.right == self.bottom && self.bottom == self.left
    }

    /// The sides in CSS order: top, right, bottom, left.
    #[must_use]
    pub const fn sides(&self) -> [i64; 4] {
        [self.top, self.right, self.bottom, self.left]
    }

    /// Build from sides in CSS order: top, right, bottom, left.
    #[must_use]
    pub const fn from_sides([top, right, bottom, left]: [i64; 4]) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    /// A copy with one side (CSS order index `0..4`) replaced.
    #[must_use]
    pub fn with_side(&self, index: usize, px: i64) -> Self {
        let mut sides = self.sides();
        if let Some(slot) = sides.get_mut(index) {
            *slot = px;
        }
        Self::from_sides(sides)
    }

    /// Parse CSS shorthand: one to four integers separated by whitespace and/or
    /// commas, expanded exactly like CSS `margin`.
    ///
    /// # Errors
    ///
    /// Returns [`ValueParseError::CssGap`] for zero or more than four values, or
    /// a value that is not an integer.
    pub fn from_hyprland_str(input: &str) -> Result<Self, ValueParseError> {
        let err = |reason: &'static str| ValueParseError::CssGap {
            input: input.to_string(),
            reason,
        };
        let cleaned = input.replace(',', " ");
        let mut values = Vec::with_capacity(4);
        for part in cleaned.split_whitespace() {
            // Hyprland stores gaps as integers, but a float literal is a common
            // hand-written slip; round rather than reject it.
            let value = match part.parse::<i64>() {
                Ok(v) => v,
                Err(_) => part
                    .parse::<f64>()
                    .ok()
                    .filter(|f| f.is_finite())
                    .map(|f| f.round() as i64)
                    .ok_or_else(|| err("gaps must be whole numbers"))?,
            };
            values.push(value);
        }
        let sides = match values.as_slice() {
            [a] => [*a, *a, *a, *a],
            [v, h] => [*v, *h, *v, *h],
            [t, h, b] => [*t, *h, *b, *h],
            [t, r, b, l] => [*t, *r, *b, *l],
            [] => return Err(err("expected one to four numbers")),
            _ => return Err(err("at most four values (top right bottom left)")),
        };
        Ok(Self::from_sides(sides))
    }

    /// Render as the shortest equivalent CSS shorthand (`5`, `5 10`, ...).
    #[must_use]
    pub fn to_hyprland_string(&self) -> String {
        let [t, r, b, l] = self.sides();
        if self.is_uniform() {
            t.to_string()
        } else if t == b && r == l {
            format!("{t} {r}")
        } else if r == l {
            format!("{t} {r} {b}")
        } else {
            format!("{t} {r} {b} {l}")
        }
    }
}

impl fmt::Display for CssGap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hyprland_string())
    }
}

/// A concrete, *scalar* configuration value.
///
/// Repeatable/structured constructs (keybinds, rules, ...) are **not** modelled
/// here; see [`crate::structured`]. Every [`Value`] variant corresponds to a
/// scalar [`crate::schema::ValueType`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A boolean.
    Bool(bool),
    /// A signed integer.
    Int(i64),
    /// A floating-point number.
    Float(f64),
    /// A single color.
    Color(Color),
    /// A color gradient.
    Gradient(Gradient),
    /// Free-form text.
    String(String),
    /// The selected variant name of an enumerated option.
    Enum(String),
    /// A 2D vector.
    Vec2(Vec2),
    /// Per-side gaps (CSS shorthand).
    CssGap(CssGap),
}

impl Value {
    /// A short, stable name for the value's kind, for diagnostics.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Color(_) => "color",
            Value::Gradient(_) => "gradient",
            Value::String(_) => "string",
            Value::Enum(_) => "enum",
            Value::Vec2(_) => "vec2",
            Value::CssGap(_) => "gap",
        }
    }
}

/// Decode a fixed number of hex bytes from a string of exactly `2 * N` hex
/// digits. Returns `None` on any length/charset mismatch.
fn hex_bytes<const N: usize>(hex: &str) -> Option<[u8; N]> {
    if hex.len() != N * 2 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (slot, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let text = std::str::from_utf8(pair).ok()?;
        *slot = u8::from_str_radix(text, 16).ok()?;
    }
    Some(out)
}

/// Format an `f64` without a trailing `.0` for whole numbers, so angles and
/// vectors round-trip as `45deg` rather than `45deg` vs `45.0deg` ambiguity.
fn format_f64(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_parses_rgba_hex() {
        let c = Color::from_hyprland_str("rgba(1a2b3cdd)").unwrap();
        assert_eq!(c, Color::rgba(0x1a, 0x2b, 0x3c, 0xdd));
    }

    #[test]
    fn color_parses_rgb_hex_as_opaque() {
        let c = Color::from_hyprland_str("rgb(1a2b3c)").unwrap();
        assert_eq!(c, Color::rgba(0x1a, 0x2b, 0x3c, 0xff));
    }

    #[test]
    fn color_parses_legacy_0x_argb() {
        // 0xAARRGGBB
        let c = Color::from_hyprland_str("0xee1a2b3c").unwrap();
        assert_eq!(c, Color::rgba(0x1a, 0x2b, 0x3c, 0xee));
    }

    #[test]
    fn color_parses_0x_rgb() {
        let c = Color::from_hyprland_str("0x1a2b3c").unwrap();
        assert_eq!(c, Color::rgba(0x1a, 0x2b, 0x3c, 0xff));
    }

    #[test]
    fn color_formats_to_rgba() {
        assert_eq!(
            Color::rgba(0x1a, 0x2b, 0x3c, 0xdd).to_rgba_string(),
            "rgba(1a2b3cdd)"
        );
        assert_eq!(Color::rgb(0, 255, 0).to_string(), "rgba(00ff00ff)");
    }

    #[test]
    fn color_round_trips() {
        for s in ["rgba(11223344)", "rgba(ffffffff)", "rgba(00000000)"] {
            let c = Color::from_hyprland_str(s).unwrap();
            assert_eq!(c.to_rgba_string(), s);
        }
    }

    #[test]
    fn color_rejects_garbage() {
        assert!(Color::from_hyprland_str("nope").is_err());
        assert!(Color::from_hyprland_str("rgba(123)").is_err());
        assert!(Color::from_hyprland_str("rgb(gg0000)").is_err());
        assert!(Color::from_hyprland_str("0x12345").is_err());
    }

    #[test]
    fn hsv_known_conversions() {
        assert_eq!(Color::from_hsv(0.0, 1.0, 1.0, 255), Color::rgb(255, 0, 0));
        assert_eq!(Color::from_hsv(120.0, 1.0, 1.0, 255), Color::rgb(0, 255, 0));
        assert_eq!(Color::from_hsv(240.0, 1.0, 1.0, 255), Color::rgb(0, 0, 255));
        assert_eq!(
            Color::from_hsv(0.0, 0.0, 1.0, 255),
            Color::rgb(255, 255, 255)
        );
        assert_eq!(
            Color::from_hsv(0.0, 0.0, 0.0, 200),
            Color::rgba(0, 0, 0, 200)
        );
        // hue wraps, alpha is carried through.
        assert_eq!(
            Color::from_hsv(360.0, 1.0, 1.0, 128),
            Color::rgba(255, 0, 0, 128)
        );

        let (h, s, v) = Color::rgb(255, 0, 0).to_hsv();
        assert!((h - 0.0).abs() < 1e-6 && (s - 1.0).abs() < 1e-6 && (v - 1.0).abs() < 1e-6);
        let (h, _, _) = Color::rgb(0, 0, 255).to_hsv();
        assert!((h - 240.0).abs() < 1e-6);
    }

    #[test]
    fn hsv_round_trips_through_picker_space() {
        // Any color reachable from HSV must survive a to_hsv → from_hsv round trip.
        for c in [
            Color::rgb(123, 45, 67),
            Color::rgb(10, 200, 240),
            Color::rgb(255, 255, 255),
            Color::rgb(0, 0, 0),
            Color::rgb(64, 128, 192),
        ] {
            let (h, s, v) = c.to_hsv();
            assert_eq!(Color::from_hsv(h, s, v, c.a), c, "round trip {c}");
        }
    }

    #[test]
    fn gradient_parses_single_color() {
        let g = Gradient::from_hyprland_str("rgba(11223344)").unwrap();
        assert_eq!(g, Gradient::solid(Color::rgba(0x11, 0x22, 0x33, 0x44)));
        assert_eq!(g.angle_deg, None);
    }

    #[test]
    fn gradient_parses_multiple_colors_and_angle() {
        let g = Gradient::from_hyprland_str("rgba(11223344) rgba(aabbccdd) 45deg").unwrap();
        assert_eq!(g.stops.len(), 2);
        assert_eq!(g.angle_deg, Some(45.0));
    }

    #[test]
    fn gradient_round_trips() {
        let input = "rgba(11223344) rgba(aabbccdd) 45deg";
        let g = Gradient::from_hyprland_str(input).unwrap();
        assert_eq!(g.to_hyprland_string(), input);
    }

    #[test]
    fn gradient_formats_integer_angle_without_decimal() {
        let g = Gradient::with_angle(vec![Color::rgb(0, 0, 0)], 90.0);
        assert_eq!(g.to_hyprland_string(), "rgba(000000ff) 90deg");
    }

    #[test]
    fn gradient_rejects_empty_and_angle_only() {
        assert!(Gradient::from_hyprland_str("").is_err());
        assert!(Gradient::from_hyprland_str("45deg").is_err());
    }

    #[test]
    fn vec2_parses_space_and_comma() {
        assert_eq!(Vec2::from_hyprland_str("3 4").unwrap(), Vec2::new(3.0, 4.0));
        assert_eq!(
            Vec2::from_hyprland_str("3, 4").unwrap(),
            Vec2::new(3.0, 4.0)
        );
        assert_eq!(
            Vec2::from_hyprland_str("1.5 -2.5").unwrap(),
            Vec2::new(1.5, -2.5)
        );
    }

    #[test]
    fn vec2_formats_and_round_trips() {
        assert_eq!(Vec2::new(0.0, 0.0).to_hyprland_string(), "0 0");
        assert_eq!(Vec2::new(1.5, -2.0).to_hyprland_string(), "1.5 -2");
        let v = Vec2::from_hyprland_str("10 20").unwrap();
        assert_eq!(v.to_hyprland_string(), "10 20");
    }

    #[test]
    fn vec2_rejects_wrong_arity() {
        assert!(Vec2::from_hyprland_str("3").is_err());
        assert!(Vec2::from_hyprland_str("3 4 5").is_err());
        assert!(Vec2::from_hyprland_str("a b").is_err());
    }

    #[test]
    fn css_gap_expands_shorthand_like_css() {
        let g = |s: &str| CssGap::from_hyprland_str(s).unwrap().sides();
        assert_eq!(g("5"), [5, 5, 5, 5]);
        assert_eq!(g("5 10"), [5, 10, 5, 10]);
        assert_eq!(g("5 10 15"), [5, 10, 15, 10]);
        assert_eq!(g("5 10 15 20"), [5, 10, 15, 20]);
        assert_eq!(g("5, 10"), [5, 10, 5, 10]);
        assert_eq!(g("2.6"), [3, 3, 3, 3]);
    }

    #[test]
    fn css_gap_renders_the_shortest_equivalent() {
        for (input, canonical) in [
            ("5", "5"),
            ("5 5 5 5", "5"),
            ("5 10 5 10", "5 10"),
            ("5 10 15 10", "5 10 15"),
            ("5 10 15 20", "5 10 15 20"),
        ] {
            let gap = CssGap::from_hyprland_str(input).unwrap();
            assert_eq!(gap.to_hyprland_string(), canonical, "{input}");
            assert_eq!(CssGap::from_hyprland_str(canonical).unwrap(), gap);
        }
    }

    #[test]
    fn css_gap_rejects_garbage() {
        assert!(CssGap::from_hyprland_str("").is_err());
        assert!(CssGap::from_hyprland_str("1 2 3 4 5").is_err());
        assert!(CssGap::from_hyprland_str("wide").is_err());
    }

    #[test]
    fn css_gap_side_edits_keep_the_rest() {
        let gap = CssGap::uniform(4).with_side(1, 9);
        assert_eq!(gap.sides(), [4, 9, 4, 4]);
        assert!(!gap.is_uniform());
        assert!(CssGap::uniform(3).is_uniform());
    }

    #[test]
    fn value_type_names_are_stable() {
        assert_eq!(Value::Bool(true).type_name(), "bool");
        assert_eq!(Value::Vec2(Vec2::new(0.0, 0.0)).type_name(), "vec2");
        assert_eq!(Value::Enum("dwindle".into()).type_name(), "enum");
    }
}

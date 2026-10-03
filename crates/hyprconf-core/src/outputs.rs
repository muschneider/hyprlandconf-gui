// SPDX-License-Identifier: MIT OR Apache-2.0
//! The displays actually attached to a running compositor.
//!
//! [`hyprctl::monitors`](crate::hyprctl::monitors) shells out to
//! `hyprctl monitors all -j`; this module owns the typed model and the parsing,
//! so both can be tested without a running Hyprland.
//!
//! Everything here describes *hardware state*, never configuration. The GUI
//! pairs a [`DetectedMonitor`] with the [`MonitorRule`](crate::structured::MonitorRule)
//! that governs it (if any) — a monitor with no rule is simply running at its
//! compositor-chosen defaults.

use serde::Deserialize;

/// A display attached to the running compositor.
///
/// Fields mirror `hyprctl monitors all -j`. Unknown/absent keys degrade to
/// defaults rather than failing the whole parse, so a newer or older Hyprland
/// still yields usable data.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedMonitor {
    /// Connector name, e.g. `DP-1`. The primary key for matching rules.
    pub name: String,
    /// Full EDID description, e.g. `Dell Inc. AW3423DWF 0x0000ABCD`. Matched by
    /// `desc:` rule selectors.
    pub description: String,
    /// Manufacturer, e.g. `Dell Inc.`.
    pub make: String,
    /// Model, e.g. `AW3423DWF`.
    pub model: String,
    /// Serial as reported by the EDID (often a hex blob, sometimes empty).
    pub serial: String,
    /// Current mode width in physical pixels.
    pub width: u32,
    /// Current mode height in physical pixels.
    pub height: u32,
    /// Current refresh rate in Hz.
    pub refresh_rate: f64,
    /// Current x position in the layout, in logical pixels.
    pub x: i32,
    /// Current y position in the layout, in logical pixels.
    pub y: i32,
    /// Current scale factor.
    pub scale: f64,
    /// Current transform (0-7), matching Hyprland's `transform` values.
    pub transform: u8,
    /// Whether this output currently holds focus.
    pub focused: bool,
    /// Whether the compositor has this output disabled.
    pub disabled: bool,
    /// Whether adaptive sync is currently active.
    pub vrr: bool,
    /// The pixel format in use, e.g. `XRGB8888` or `XRGB2101010`.
    pub current_format: String,
    /// The output this one mirrors, if any (`none` in the JSON becomes `None`).
    pub mirror_of: Option<String>,
    /// Every mode the display advertises, best first as reported.
    pub modes: Vec<Mode>,
    /// Reported HDR support, when the compositor exposes it.
    pub supports_hdr: Option<bool>,
    /// Reported wide-colour support, when the compositor exposes it.
    pub supports_wide_color: Option<bool>,
}

impl DetectedMonitor {
    /// A human label: `Dell Inc. AW3423DWF`, falling back to the description
    /// and finally the connector so this is never empty.
    #[must_use]
    pub fn label(&self) -> String {
        let combined = format!("{} {}", self.make.trim(), self.model.trim());
        let combined = combined.trim();
        if !combined.is_empty() {
            return combined.to_string();
        }
        if !self.description.trim().is_empty() {
            return self.description.trim().to_string();
        }
        self.name.clone()
    }

    /// The current mode as a Hyprland mode string, e.g. `3440x1440@164.90`.
    #[must_use]
    pub fn current_mode(&self) -> String {
        Mode {
            width: self.width,
            height: self.height,
            refresh: self.refresh_rate,
        }
        .to_hyprland()
    }

    /// The current position as a Hyprland position string, e.g. `1920x0`.
    #[must_use]
    pub fn current_position(&self) -> String {
        format!("{}x{}", self.x, self.y)
    }

    /// The `desc:` selector that matches this output.
    #[must_use]
    pub fn desc_selector(&self) -> String {
        format!("desc:{}", self.description)
    }

    /// Capability tags worth badging in the UI.
    ///
    /// Deliberately conservative: only claims what the compositor actually
    /// reports. `HDR`/`10-bit` come from explicit capability flags when the
    /// running Hyprland exposes them, otherwise `10-bit` is inferred from a
    /// 30-bit pixel format actually in use. Nothing is guessed from the model
    /// name.
    #[must_use]
    pub fn capabilities(&self) -> Vec<&'static str> {
        let mut caps = Vec::new();
        if self.supports_hdr == Some(true) {
            caps.push("HDR");
        }
        if self.supports_wide_color == Some(true) || is_ten_bit(&self.current_format) {
            caps.push("10-bit");
        }
        if self.vrr {
            caps.push("VRR");
        }
        caps
    }

    /// The size this output occupies in the layout, in logical pixels, honouring
    /// both scale and a rotating transform (which swaps width and height).
    #[must_use]
    pub fn logical_size(&self) -> (f32, f32) {
        logical_size(self.width, self.height, self.scale, self.transform)
    }
}

/// Whether a Hyprland pixel format name denotes 30-bit colour.
fn is_ten_bit(format: &str) -> bool {
    format.contains("2101010")
}

/// The logical (post-scale, post-rotation) size of a mode.
///
/// Transforms 1/3/5/7 are the 90°/270° rotations, which swap the axes. A
/// non-finite or non-positive scale is treated as 1.0 rather than producing a
/// degenerate rectangle.
#[must_use]
pub fn logical_size(width: u32, height: u32, scale: f64, transform: u8) -> (f32, f32) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let w = f64::from(width) / scale;
    let h = f64::from(height) / scale;
    let (w, h) = if matches!(transform, 1 | 3 | 5 | 7) {
        (h, w)
    } else {
        (w, h)
    };
    (w as f32, h as f32)
}

/// A display mode: resolution plus refresh rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mode {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Refresh rate in Hz.
    pub refresh: f64,
}

impl Mode {
    /// Parse one `availableModes` entry, e.g. `1920x1080@74.97Hz`.
    ///
    /// The trailing `Hz` is optional so this also accepts the mode syntax used
    /// in `monitor =` rules.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (size, rate) = text.split_once('@')?;
        let (w, h) = size.split_once(['x', 'X'])?;
        let rate = rate.trim().trim_end_matches("Hz").trim_end_matches("hz");
        Some(Self {
            width: w.trim().parse().ok()?,
            height: h.trim().parse().ok()?,
            refresh: rate.trim().parse().ok()?,
        })
    }

    /// The mode as Hyprland's `monitor =` rule spells it: `1920x1080@74.97`.
    ///
    /// Two decimals matter: Hyprland picks the closest advertised mode, and
    /// 59.94 vs 60.00 are genuinely different modes on many displays.
    #[must_use]
    pub fn to_hyprland(self) -> String {
        format!("{}x{}@{:.2}", self.width, self.height, self.refresh)
    }

    /// A readable label for a dropdown: `1920x1080 @ 74.97 Hz`.
    #[must_use]
    pub fn label(self) -> String {
        format!("{}x{} @ {:.2} Hz", self.width, self.height, self.refresh)
    }

    /// Pixel count, for ordering modes by "biggest first".
    #[must_use]
    pub fn pixels(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

// ---------------------------------------------------------------------------
// parsing
// ---------------------------------------------------------------------------

/// The subset of `hyprctl monitors all -j` we consume.
///
/// Every field is defaulted: Hyprland's output grows between releases, and a
/// missing key must never cost us the whole list.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawMonitor {
    name: String,
    description: String,
    make: String,
    model: String,
    serial: String,
    width: u32,
    height: u32,
    refresh_rate: f64,
    x: i32,
    y: i32,
    scale: f64,
    transform: u8,
    focused: bool,
    disabled: bool,
    vrr: bool,
    current_format: String,
    mirror_of: String,
    available_modes: Vec<String>,
    #[serde(rename = "supportsHDR")]
    supports_hdr: Option<bool>,
    supports_wide_color: Option<bool>,
}

impl Default for RawMonitor {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            make: String::new(),
            model: String::new(),
            serial: String::new(),
            width: 0,
            height: 0,
            refresh_rate: 0.0,
            x: 0,
            y: 0,
            // A zero scale would divide the layout by zero; 1.0 is the only
            // sane stand-in for "not reported".
            scale: 1.0,
            transform: 0,
            focused: false,
            disabled: false,
            vrr: false,
            current_format: String::new(),
            mirror_of: String::new(),
            available_modes: Vec::new(),
            supports_hdr: None,
            supports_wide_color: None,
        }
    }
}

/// Parse the JSON array printed by `hyprctl monitors all -j`.
///
/// # Errors
///
/// Returns the `serde_json` message if the text is not a JSON array of objects.
pub fn parse_monitors(json: &str) -> Result<Vec<DetectedMonitor>, String> {
    let raw: Vec<RawMonitor> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(raw.into_iter().map(convert).collect())
}

fn convert(raw: RawMonitor) -> DetectedMonitor {
    let mut modes: Vec<Mode> = raw
        .available_modes
        .iter()
        .filter_map(|m| Mode::parse(m))
        .collect();
    // Biggest and fastest first: the list is a menu, and the mode a user wants
    // is far more often the panel's best than its worst.
    modes.sort_by(|a, b| {
        b.pixels().cmp(&a.pixels()).then_with(|| {
            b.refresh
                .partial_cmp(&a.refresh)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    modes.dedup_by(|a, b| a.width == b.width && a.height == b.height && a.refresh == b.refresh);

    DetectedMonitor {
        name: raw.name,
        description: raw.description,
        make: raw.make,
        model: raw.model,
        serial: raw.serial,
        width: raw.width,
        height: raw.height,
        refresh_rate: raw.refresh_rate,
        x: raw.x,
        y: raw.y,
        scale: raw.scale,
        transform: raw.transform,
        focused: raw.focused,
        disabled: raw.disabled,
        vrr: raw.vrr,
        current_format: raw.current_format,
        mirror_of: match raw.mirror_of.trim() {
            "" | "none" => None,
            other => Some(other.to_string()),
        },
        modes,
        supports_hdr: raw.supports_hdr,
        supports_wide_color: raw.supports_wide_color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[{
        "id": 0, "name": "DP-1",
        "description": "Dell Inc. AW3423DWF 0x0000ABCD",
        "make": "Dell Inc.", "model": "AW3423DWF", "serial": "0x0000ABCD",
        "width": 3440, "height": 1440, "refreshRate": 164.90001,
        "x": 0, "y": 0, "scale": 1.00, "transform": 0,
        "focused": true, "dpmsStatus": true, "vrr": true, "disabled": false,
        "currentFormat": "XRGB2101010", "mirrorOf": "none",
        "availableModes": ["3440x1440@60.00Hz","3440x1440@164.90Hz","1920x1080@60.00Hz"]
    },{
        "id": 1, "name": "DP-3",
        "description": "Samsung Electric Company LF24T35 0x00001111",
        "make": "Samsung Electric Company", "model": "LF24T35", "serial": "",
        "width": 1920, "height": 1080, "refreshRate": 74.973,
        "x": 3440, "y": 0, "scale": 1.00, "transform": 3,
        "focused": false, "vrr": false, "disabled": false,
        "currentFormat": "XRGB8888", "mirrorOf": "DP-1",
        "availableModes": ["1920x1080@74.97Hz"]
    }]"#;

    #[test]
    fn parses_a_real_two_monitor_payload() {
        let monitors = parse_monitors(SAMPLE).unwrap();
        assert_eq!(monitors.len(), 2);

        let first = &monitors[0];
        assert_eq!(first.name, "DP-1");
        assert_eq!(first.label(), "Dell Inc. AW3423DWF");
        assert_eq!(first.current_mode(), "3440x1440@164.90");
        assert_eq!(first.current_position(), "0x0");
        assert_eq!(first.desc_selector(), "desc:Dell Inc. AW3423DWF 0x0000ABCD");
        assert!(first.mirror_of.is_none());

        let second = &monitors[1];
        assert_eq!(second.mirror_of.as_deref(), Some("DP-1"));
        assert_eq!(second.current_position(), "3440x0");
    }

    #[test]
    fn modes_are_sorted_best_first_and_deduplicated() {
        let monitors = parse_monitors(SAMPLE).unwrap();
        let labels: Vec<String> = monitors[0].modes.iter().map(|m| m.to_hyprland()).collect();
        assert_eq!(
            labels,
            ["3440x1440@164.90", "3440x1440@60.00", "1920x1080@60.00"]
        );
    }

    #[test]
    fn capabilities_are_only_claimed_when_reported() {
        let monitors = parse_monitors(SAMPLE).unwrap();
        // 30-bit format in use + adaptive sync active; HDR is never guessed.
        assert_eq!(monitors[0].capabilities(), ["10-bit", "VRR"]);
        assert!(monitors[1].capabilities().is_empty());

        // …but an explicit flag from a newer Hyprland is honoured.
        let hdr = parse_monitors(r#"[{"name":"DP-1","supportsHDR":true}]"#).unwrap();
        assert_eq!(hdr[0].capabilities(), ["HDR"]);
    }

    #[test]
    fn missing_fields_degrade_instead_of_failing() {
        let monitors = parse_monitors(r#"[{"name":"DP-9"}]"#).unwrap();
        assert_eq!(monitors.len(), 1);
        assert_eq!(monitors[0].label(), "DP-9", "falls back to the connector");
        assert_eq!(monitors[0].scale, 1.0, "a missing scale must not be zero");
        assert!(monitors[0].modes.is_empty());
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(parse_monitors("not json").is_err());
        assert!(parse_monitors("{}").is_err());
        assert!(parse_monitors("[]").unwrap().is_empty());
    }

    #[test]
    fn mode_parsing_accepts_both_spellings() {
        assert_eq!(
            Mode::parse("1920x1080@74.97Hz"),
            Some(Mode {
                width: 1920,
                height: 1080,
                refresh: 74.97
            })
        );
        assert_eq!(
            Mode::parse("1920x1080@144"),
            Some(Mode {
                width: 1920,
                height: 1080,
                refresh: 144.0
            })
        );
        assert_eq!(Mode::parse("preferred"), None);
        assert_eq!(Mode::parse("1920x1080"), None);
    }

    #[test]
    fn rotated_and_scaled_outputs_report_swapped_logical_sizes() {
        assert_eq!(logical_size(1920, 1080, 1.0, 0), (1920.0, 1080.0));
        assert_eq!(logical_size(1920, 1080, 1.0, 3), (1080.0, 1920.0));
        assert_eq!(logical_size(3840, 2160, 2.0, 0), (1920.0, 1080.0));
        // A nonsense scale must not produce an infinite rectangle.
        assert_eq!(logical_size(1920, 1080, 0.0, 0), (1920.0, 1080.0));
    }
}

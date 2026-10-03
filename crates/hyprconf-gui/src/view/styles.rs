// SPDX-License-Identifier: MIT OR Apache-2.0
//! Theme-aware widget styling for the views.
//!
//! Pure functions from a `&Theme` (and, where relevant, a widget status) to
//! the matching iced `Style`. Kept in their own module so the view modules
//! stay focused on layout rather than palette plumbing.

use iced::widget::{button, container, text, text_input};
use iced::{Background, Border, Color, Theme};

use hyprconf_core::Severity;

pub(super) fn accent(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.extended_palette().primary.base.color),
    }
}

pub(super) fn muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.palette().text.scale_alpha(0.6)),
    }
}

pub(super) fn danger(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.extended_palette().danger.base.color),
    }
}

pub(super) fn success(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.extended_palette().success.base.color),
    }
}

/// Warning amber. Theme-aware: a brighter amber on dark backgrounds, a deeper
/// one on light backgrounds, so it stays legible and never reads as an error
/// (which is reserved for [`danger`]).
pub(super) fn warn_style(theme: &Theme) -> text::Style {
    let bg = theme.palette().background;
    // Perceived luminance of the background (Rec. 601).
    let luminance = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    let color = if luminance < 0.5 {
        Color::from_rgb8(0xE6, 0xAE, 0x36)
    } else {
        Color::from_rgb8(0x8A, 0x5D, 0x00)
    };
    text::Style { color: Some(color) }
}

/// The glyph used for a severity, kept consistent everywhere severities are
/// shown inline: a cross for errors, a triangle for warnings.
pub(super) fn severity_icon(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "✕",
        Severity::Warning => "⚠",
    }
}

/// The text colour for a severity: [`danger`] (red) for errors, [`warn_style`]
/// (amber) for warnings.
pub(super) fn severity_style(severity: Severity) -> fn(&Theme) -> text::Style {
    match severity {
        Severity::Error => danger,
        Severity::Warning => warn_style,
    }
}

/// Background for the diff/code block.
pub(super) fn code_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.weakest.color.into()),
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// Header & status bars: a panel-tinted strip.
pub(super) fn bar_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.weak.color.into()),
        ..container::Style::default()
    }
}

/// Sidebar panel.
pub(super) fn panel_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.weak.color.into()),
        ..container::Style::default()
    }
}

/// A raised card on the base background.
pub(super) fn card_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.weak.color.into()),
        border: Border {
            color: p.background.strong.color.scale_alpha(0.5),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..container::Style::default()
    }
}

/// Small count/tag badge.
pub(super) fn badge_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.strong.color.into()),
        text_color: Some(p.background.strong.text),
        border: Border {
            radius: 8.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// An "n unsaved edits here" badge on a sidebar section.
///
/// Accent-tinted rather than filled: it must be findable at a glance while
/// scanning twenty sections, without shouting louder than the selected entry.
pub(super) fn dirty_badge_style(theme: &Theme) -> container::Style {
    let c = theme.extended_palette().primary.base.color;
    container::Style {
        background: Some(c.scale_alpha(0.22).into()),
        text_color: Some(c),
        border: Border {
            radius: 8.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// The format badge in the status bar (accent-tinted).
pub(super) fn format_badge_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.primary.base.color.into()),
        text_color: Some(p.primary.base.text),
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// Sidebar nav button: accent fill when selected, subtle hover otherwise.
pub(super) fn nav_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let p = theme.extended_palette();
    let (background, text_color) = if selected {
        (Some(p.primary.base.color.into()), p.primary.base.text)
    } else {
        match status {
            button::Status::Hovered | button::Status::Pressed => (
                Some(p.background.strong.color.scale_alpha(0.5).into()),
                p.background.base.text,
            ),
            _ => (None, p.background.base.text),
        }
    };
    button::Style {
        background,
        text_color,
        border: Border {
            radius: 7.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// Search-result row: invisible until hovered.
pub(super) fn result_style(theme: &Theme, status: button::Status) -> button::Style {
    let p = theme.extended_palette();
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Some(Background::from(p.background.weak.color))
        }
        _ => None,
    };
    button::Style {
        background,
        text_color: p.background.base.text,
        border: Border {
            radius: 8.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// A subtle, borderless button (reset/remove/add controls).
pub(super) fn ghost_button(theme: &Theme, status: button::Status) -> button::Style {
    let p = theme.extended_palette();
    let (background, alpha) = match status {
        button::Status::Hovered | button::Status::Pressed => (
            Some(p.background.strong.color.scale_alpha(0.5).into()),
            0.95,
        ),
        button::Status::Disabled => (None, 0.35),
        button::Status::Active => (None, 0.8),
    };
    button::Style {
        background,
        text_color: p.background.base.text.scale_alpha(alpha),
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// A small toggle chip (modifiers, bind flags, v2): accent when active.
///
/// Inactive chips carry an outline: chips usually sit on a card, and a
/// card-coloured chip without one reads as plain text, not as a toggle.
pub(super) fn chip_style(theme: &Theme, status: button::Status, active: bool) -> button::Style {
    let p = theme.extended_palette();
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let (background, text_color, border_color) = if active {
        (
            Some(p.primary.base.color.into()),
            p.primary.base.text,
            p.primary.base.color,
        )
    } else {
        let fill = if hovered {
            Some(p.background.strong.color.into())
        } else {
            None
        };
        (
            fill,
            p.background.base.text.scale_alpha(0.85),
            p.background.strong.color,
        )
    };
    button::Style {
        background,
        text_color,
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..button::Style::default()
    }
}

/// The "N unsaved" indicator pill (accent-tinted).
pub(super) fn changes_button_style(theme: &Theme, status: button::Status) -> button::Style {
    let p = theme.extended_palette();
    let base = p.primary.base.color;
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => base,
        _ => base.scale_alpha(0.85),
    };
    button::Style {
        background: Some(background.into()),
        text_color: p.primary.base.text,
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// The floating tooltip box.
pub(super) fn tooltip_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.strong.color.into()),
        text_color: Some(p.background.strong.text),
        border: Border {
            color: p.background.stronger.color,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

/// The `.conf` deprecation banner: an amber-tinted strip with a left accent bar.
///
/// Deliberately *not* danger-red — nothing is broken yet, the user simply has a
/// deadline. Red here would cry wolf against real validation errors.
pub(super) fn banner_style(theme: &Theme) -> container::Style {
    let amber = warn_style(theme).color.unwrap_or(Color::WHITE);
    container::Style {
        background: Some(amber.scale_alpha(0.14).into()),
        border: Border {
            color: amber.scale_alpha(0.55),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..container::Style::default()
    }
}

/// A prominent call-to-action button (accent fill, used for the primary step
/// action in a flow).
pub(super) fn primary_button(theme: &Theme, status: button::Status) -> button::Style {
    let p = theme.extended_palette();
    let (background, alpha) = match status {
        button::Status::Hovered | button::Status::Pressed => (p.primary.strong.color, 1.0),
        button::Status::Disabled => (p.primary.base.color, 0.35),
        button::Status::Active => (p.primary.base.color, 1.0),
    };
    button::Style {
        background: Some(background.scale_alpha(alpha).into()),
        text_color: p.primary.base.text.scale_alpha(alpha.max(0.6)),
        border: Border {
            radius: 7.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// A success-tinted panel (the "config ok" verdict).
pub(super) fn success_panel(theme: &Theme) -> container::Style {
    let c = theme.extended_palette().success.base.color;
    container::Style {
        background: Some(c.scale_alpha(0.12).into()),
        border: Border {
            color: c.scale_alpha(0.5),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..container::Style::default()
    }
}

/// A danger-tinted panel (a failed verification).
pub(super) fn danger_panel(theme: &Theme) -> container::Style {
    let c = theme.extended_palette().danger.base.color;
    container::Style {
        background: Some(c.scale_alpha(0.12).into()),
        border: Border {
            color: c.scale_alpha(0.5),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..container::Style::default()
    }
}

/// A step pill in the migration progress indicator.
///
/// Three states matter: `done` (already passed), `current` (where the user is),
/// and upcoming. Only the current step is filled, so the eye lands on it.
pub(super) fn step_style(theme: &Theme, current: bool, done: bool) -> container::Style {
    let p = theme.extended_palette();
    let (background, text_color) = if current {
        (p.primary.base.color, p.primary.base.text)
    } else if done {
        (
            p.success.base.color.scale_alpha(0.22),
            p.background.base.text,
        )
    } else {
        (
            p.background.strong.color.scale_alpha(0.35),
            p.background.base.text.scale_alpha(0.7),
        )
    };
    container::Style {
        background: Some(background.into()),
        text_color: Some(text_color),
        border: Border {
            radius: 20.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// An inset surface for content that sits *inside* a card — the monitor layout
/// canvas, a settings row. One step away from the card so the nesting reads,
/// without inventing a colour the theme doesn't have.
pub(super) fn inset_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.strong.color.scale_alpha(0.30).into()),
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// The accent strip drawn down the left edge of the first row in a settings
/// group. Rendered as a thin accent-filled backing that the row sits on top of,
/// so it always matches the row's height exactly.
pub(super) fn accent_edge_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.extended_palette().primary.base.color.into()),
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// A capability badge (`HDR`, `10-bit`, `VRR`): accent-tinted, low-key.
///
/// Deliberately quieter than [`format_badge_style`] — these are facts about the
/// hardware, not something the user chose or needs to act on.
pub(super) fn cap_badge_style(theme: &Theme) -> container::Style {
    let c = theme.extended_palette().primary.base.color;
    container::Style {
        background: Some(c.scale_alpha(0.18).into()),
        text_color: Some(c),
        border: Border {
            color: c.scale_alpha(0.45),
            width: 1.0,
            radius: 5.0.into(),
        },
        ..container::Style::default()
    }
}

/// A neutral status badge (`Managed`, `Not connected`): outlined, no fill, so it
/// never competes with the accent badges beside it.
pub(super) fn soft_badge_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        text_color: Some(p.background.base.text.scale_alpha(0.7)),
        border: Border {
            color: p.background.strong.color,
            width: 1.0,
            radius: 5.0.into(),
        },
        ..container::Style::default()
    }
}

/// A text input that flags validation errors with a danger-colored border.
pub(super) fn input_style(
    theme: &Theme,
    status: text_input::Status,
    has_error: bool,
) -> text_input::Style {
    let mut style = text_input::default(theme, status);
    if has_error {
        style.border.color = theme.extended_palette().danger.base.color;
        style.border.width = 1.5;
    }
    style
}

/// Even quieter than [`muted`]: option paths and other reference text that
/// should be findable but never compete with the label.
pub(super) fn faint(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(
            theme
                .extended_palette()
                .background
                .base
                .text
                .scale_alpha(0.42),
        ),
    }
}

/// One segment of a joined segmented control (a row of mutually exclusive
/// choices): only the outer corners are rounded, so the row reads as a single
/// control rather than a scatter of chips.
pub(super) fn segment_style(
    theme: &Theme,
    status: button::Status,
    active: bool,
    first: bool,
    last: bool,
) -> button::Style {
    let p = theme.extended_palette();
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let (background, text_color) = if active {
        (p.primary.base.color, p.primary.base.text)
    } else if hovered {
        (p.background.strong.color, p.background.base.text)
    } else {
        (
            p.background.base.color,
            p.background.base.text.scale_alpha(0.8),
        )
    };
    let r = 7.0;
    button::Style {
        background: Some(background.into()),
        text_color,
        border: Border {
            color: p.background.strong.color,
            width: 1.0,
            radius: iced::border::Radius {
                top_left: if first { r } else { 0.0 },
                bottom_left: if first { r } else { 0.0 },
                top_right: if last { r } else { 0.0 },
                bottom_right: if last { r } else { 0.0 },
            },
        },
        ..button::Style::default()
    }
}

/// A keyboard-key cap, for keybind summaries (`SUPER` `Q`).
pub(super) fn keycap_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.base.color.into()),
        text_color: Some(p.background.base.text),
        border: Border {
            color: p.background.strong.color,
            width: 1.0,
            radius: 5.0.into(),
        },
        ..container::Style::default()
    }
}

/// The heading strip of a group card ("Blur", "Shadow", …).
pub(super) fn group_head_style(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.strong.color.scale_alpha(0.25).into()),
        border: Border {
            radius: iced::border::Radius {
                top_left: 8.0,
                top_right: 8.0,
                bottom_left: 0.0,
                bottom_right: 0.0,
            },
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// A hairline between rows inside a card.
pub(super) fn divider_style(theme: &Theme) -> iced::widget::rule::Style {
    iced::widget::rule::Style {
        color: theme
            .extended_palette()
            .background
            .strong
            .color
            .scale_alpha(0.45),
        radius: 0.0.into(),
        fill_mode: iced::widget::rule::FillMode::Full,
        snap: true,
    }
}

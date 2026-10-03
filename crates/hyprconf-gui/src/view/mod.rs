// SPDX-License-Identifier: MIT OR Apache-2.0
//! All rendering. Pure functions over `&App` producing Iced elements.

use iced::widget::{
    button, canvas, center, column, container, mouse_area, opaque, pick_list, row, scrollable,
    slider, stack, text, text_input, toggler, tooltip, Column, Space,
};
use iced::{Alignment, Border, Color, Element, Font, Length, Theme};

use hyprconf_core::schema::CollectionId;
use hyprconf_core::value::Color as HyprColor;
use hyprconf_core::{ConfigFormat, Severity, Value};

use crate::diff::Tag;
use crate::edit::{ColorChannel, EditAction, Slot};
use crate::load::{format_label, Diagnostic, LoadState, Loaded};
use crate::save::{self, SaveMode};
use crate::{color_picker, App, Message, Selection};

mod collections;
mod migrate;
mod monitor_canvas;
mod monitors;
mod nav;
mod options;
mod styles;
use styles::*;

// Shared with `monitors` (which builds its raw-rule list from these).
use collections::{chip, coll_text, monitor_row};
use nav::{collection_count, collection_icon};
use options::EnumChoice;

/// The search field's widget id, so Ctrl+F can focus it from `update`.
pub const SEARCH_ID: &str = "hyprconf-search";
const BOLD: Font = Font {
    weight: iced::font::Weight::Bold,
    ..Font::DEFAULT
};
const MONO: Font = Font::MONOSPACE;

/// The whole window: header / [sidebar | content] / status bar, with the color
/// picker floating above as a modal when open.
pub fn view(app: &App) -> Element<'_, Message> {
    let mut shell = column![header(app)];
    if app.show_deprecation_banner() {
        shell = shell.push(container(migrate::deprecation_banner(app)).padding([8, 18]));
    }
    let base: Element<Message> = shell
        .push(row![nav::sidebar(app), content(app)].height(Length::Fill))
        .push(status_bar(app))
        .into();

    if app.show_shortcuts {
        return modal(base, shortcuts_panel(), Message::ToggleShortcuts);
    }
    match &app.color_picker {
        Some(draft) => modal(
            base,
            color_picker_panel(app, draft),
            Message::CloseColorPicker,
        ),
        None => base,
    }
}

/// Every keyboard shortcut, in one sheet.
///
/// Shortcuts that nobody can discover may as well not exist; the tooltips on
/// individual buttons only cover the ones that *have* a button.
fn shortcuts_panel() -> Element<'static, Message> {
    const KEYS: [(&str, &str); 8] = [
        ("Ctrl + K / Ctrl + F", "Focus the search field"),
        ("Enter", "Open the best search hit's section"),
        ("Esc", "Back out one layer (modal → panel → search)"),
        ("Ctrl + Z", "Undo"),
        ("Ctrl + Shift + Z / Ctrl + Y", "Redo"),
        ("Ctrl + S", "Open the save panel"),
        ("Ctrl + P", "Profiles & recent files"),
        ("Ctrl + /", "This sheet"),
    ];

    let rows = Column::with_children(KEYS.map(|(keys, what)| {
        row![
            container(text(keys).size(12).font(MONO))
                .padding([2, 8])
                .style(badge_style),
            Space::new().width(Length::Fixed(14.0)),
            text(what).size(13),
        ]
        .align_y(Alignment::Center)
        .into()
    }))
    .spacing(8);

    let header = row![
        text("Keyboard shortcuts").size(16).font(BOLD),
        Space::new().width(Length::Fill),
        button(text("✕").size(14))
            .padding([2, 8])
            .on_press(Message::ToggleShortcuts)
            .style(ghost_button),
    ]
    .align_y(Alignment::Center);

    container(column![header, rows].spacing(16))
        .padding(20)
        .max_width(460.0)
        .style(card_style)
        .into()
}

/// Float `content` (a dimmed, click-to-dismiss modal) above `base`.
fn modal<'a>(
    base: Element<'a, Message>,
    content: Element<'a, Message>,
    on_blur: Message,
) -> Element<'a, Message> {
    stack![
        base,
        opaque(
            mouse_area(center(opaque(content)).style(|_theme| {
                container::Style {
                    background: Some(
                        Color {
                            a: 0.7,
                            ..Color::BLACK
                        }
                        .into(),
                    ),
                    ..container::Style::default()
                }
            }))
            .on_press(on_blur)
        ),
    ]
    .into()
}

// ---------------------------------------------------------------------------
// header
// ---------------------------------------------------------------------------

fn header(app: &App) -> Element<'_, Message> {
    let brand = row![
        text("❖").size(22).style(accent),
        text("hyprconf").size(20).font(BOLD),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    // The search field is the only elastic element, but it must not be squeezed
    // to nothing by the toolbar on a narrow window — below ~1080px it stops
    // growing and the toolbar wraps into icon-only form instead.
    let search = text_input("Search settings…  (Ctrl+K)", &app.search)
        .id(SEARCH_ID)
        .on_input(Message::SearchChanged)
        // Enter jumps to the best hit's section, so a search can be finished
        // entirely from the keyboard.
        .on_submit(match app.hits.options.first() {
            Some(hit) => Message::Selected(Selection::Section(hit.section.to_string())),
            None => Message::Escape,
        })
        .padding([8, 12])
        .size(15)
        .width(Length::Fill);

    // A clear button only when there is something to clear — an always-present
    // "✕" next to an empty field is just noise.
    let search: Element<Message> = if app.search.is_empty() {
        search.into()
    } else {
        row![
            search,
            button(text("✕").size(12))
                .padding([6, 10])
                .on_press(Message::Escape)
                .style(ghost_button),
        ]
        .spacing(4)
        .align_y(Alignment::Center)
        .into()
    };

    // The dropdown already shows the theme name, so the "Theme" label was
    // redundant width in the most contested row of the UI.
    let theme_picker = pick_list(Theme::ALL, Some(app.theme.clone()), Message::ThemeSelected)
        .text_size(13)
        .padding([6, 10])
        .width(Length::Fixed(160.0));

    let mut bar = row![brand, search].spacing(16).align_y(Alignment::Center);
    if let Some(changes) = changes_indicator(app) {
        bar = bar.push(changes);
    }
    if app.load.loaded().is_some() {
        // Keep the migration reachable after the banner has been dismissed —
        // otherwise dismissing it hides the feature for the whole session.
        if app
            .load
            .loaded()
            .is_some_and(|l| crate::migrate::should_warn(l.format))
            && app.migration.is_none()
        {
            bar = bar.push(
                button(text("→ Lua").size(13))
                    .padding([6, 12])
                    .on_press(Message::StartMigration)
                    .style(primary_button),
            );
        }
        bar = bar.push(history_button(
            "↶",
            "Undo (Ctrl+Z)",
            (!app.undo.is_empty()).then_some(Message::Undo),
        ));
        bar = bar.push(history_button(
            "↷",
            "Redo (Ctrl+Shift+Z)",
            (!app.redo.is_empty()).then_some(Message::Redo),
        ));
        bar = bar.push(toolbar_toggle(
            "profiles",
            app.show_profiles,
            Message::ToggleProfiles,
        ));
        bar = bar.push(toolbar_toggle("save…", app.show_save, Message::ToggleSave));
    }
    bar = bar.push(history_button(
        "?",
        "Keyboard shortcuts (Ctrl+/)",
        Some(Message::ToggleShortcuts),
    ));
    bar = bar.push(theme_picker);

    container(bar)
        .padding([12, 18])
        .width(Length::Fill)
        .style(bar_style)
        .into()
}

/// The clickable "N unsaved" pill (only when a config is loaded).
fn changes_indicator(app: &App) -> Option<Element<'_, Message>> {
    let loaded = app.load.loaded()?;
    let count = loaded.total_unsaved();
    let (label, kind) = if count == 0 {
        ("no changes".to_string(), false)
    } else {
        (format!("● {count} unsaved"), true)
    };
    let style: fn(&Theme, button::Status) -> button::Style = if kind {
        changes_button_style
    } else {
        ghost_button
    };
    Some(
        button(text(label).size(13))
            .padding([6, 12])
            .on_press(Message::ToggleChanges)
            .style(style)
            .into(),
    )
}

/// A small icon button with a tooltip; disabled when `message` is `None`.
fn history_button(
    glyph: &'static str,
    tip: &'static str,
    message: Option<Message>,
) -> Element<'static, Message> {
    let mut b = button(text(glyph).size(15))
        .padding([6, 11])
        .style(ghost_button);
    if let Some(m) = message {
        b = b.on_press(m);
    }
    tooltip(
        b,
        container(text(tip).size(12))
            .padding([6, 10])
            .style(tooltip_style),
        tooltip::Position::Bottom,
    )
    .into()
}

/// A header toggle button (accent-tinted while its panel is open).
fn toolbar_toggle(
    label: &'static str,
    active: bool,
    message: Message,
) -> Element<'static, Message> {
    let style: fn(&Theme, button::Status) -> button::Style = if active {
        changes_button_style
    } else {
        ghost_button
    };
    button(text(label).size(13))
        .padding([6, 12])
        .on_press(message)
        .style(style)
        .into()
}

// ---------------------------------------------------------------------------
// content area
// ---------------------------------------------------------------------------

fn content(app: &App) -> Element<'_, Message> {
    // The migration flow takes the whole pane: it is a linear, commit-oriented
    // task, and leaving the editor visible alongside it invites half-finished
    // edits that the already-generated Lua would not include.
    if let Some(m) = &app.migration {
        return container(migrate::migrate_view(app, m))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(20)
            .into();
    }
    if app.show_profiles {
        return container(profiles_view(app))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(20)
            .into();
    }
    let inner: Element<Message> = match &app.load {
        LoadState::Loading => centered(text("Loading configuration…").size(16).style(muted)),
        LoadState::NotFound { searched } => not_found_view(searched),
        LoadState::Error { path, message } => error_view(&path.display().to_string(), message),
        LoadState::Loaded(loaded) => {
            if app.show_diagnostics {
                diagnostics_view(loaded)
            } else if app.show_save {
                // The preview is precomputed in `update`; `None` only if a
                // refresh was somehow missed, so degrade gracefully.
                match &app.save_preview {
                    Some(preview) => save_view(app, loaded, preview),
                    None => centered(text("Preparing save preview…").size(16).style(muted)),
                }
            } else if app.show_changes {
                changes_view(app, loaded)
            } else if app.search.trim().is_empty() {
                match &app.selected {
                    Selection::Section(id) => options::section_view(app, loaded, id),
                    // Monitors is the one collection that isn't really a list of
                    // text: it gets a hardware-driven screen of its own.
                    Selection::Collection(CollectionId::Monitors) => monitors::view(app, loaded),
                    Selection::Collection(id) => collections::collection_view(app, loaded, *id),
                }
            } else {
                options::search_results(app, loaded)
            }
        }
    };

    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(20)
        .into()
}

fn centered(content: impl Into<Element<'static, Message>>) -> Element<'static, Message> {
    container(content.into()).center(Length::Fill).into()
}

fn not_found_view(searched: &[std::path::PathBuf]) -> Element<'static, Message> {
    let mut lines: Vec<Element<Message>> = vec![
        text("🔍").size(40).into(),
        text("No Hyprland configuration found")
            .size(20)
            .font(BOLD)
            .into(),
        text("Looked for:").size(13).style(muted).into(),
    ];
    for path in searched {
        lines.push(text(format!("• {}", path.display())).size(13).into());
    }
    lines.push(Space::new().height(Length::Fixed(8.0)).into());
    lines.push(
        text("Pass --config <path> to load a specific file.")
            .size(13)
            .style(muted)
            .into(),
    );
    centered(
        Column::with_children(lines)
            .spacing(8)
            .align_x(Alignment::Center),
    )
}

fn error_view(path: &str, message: &str) -> Element<'static, Message> {
    let body = column![
        text("⚠").size(40).style(danger),
        text("Couldn’t load this configuration").size(20).font(BOLD),
        text(path.to_string()).size(13).font(MONO).style(muted),
        container(text(format!("Reason: {message}")).size(13).style(danger)).max_width(560.0),
        Space::new().height(Length::Fixed(6.0)),
        container(
            text(
                "Fix the problem above in your editor and reopen, or start hyprconf with \
                 “--config <path>” to open a different file. Your file was not modified.",
            )
            .size(13)
            .style(muted),
        )
        .max_width(560.0),
    ]
    .spacing(8)
    .align_x(Alignment::Center);
    centered(body)
}

/// A pane's title card. `title`/`subtitle` are borrowed — most callers pass
/// `&'static Schema` strings, and cloning them on every frame was pure waste.
fn pane_header<'a>(
    icon: &'static str,
    title: impl text::IntoFragment<'a>,
    subtitle: impl text::IntoFragment<'a>,
    trailing: Element<'a, Message>,
) -> Element<'a, Message> {
    let left = row![
        text(icon).size(26),
        column![
            text(title).size(22).font(BOLD),
            text(subtitle).size(13).style(muted),
        ]
        .spacing(2),
    ]
    .spacing(12)
    .align_y(Alignment::Center);

    container(row![left.width(Length::Fill), trailing].align_y(Alignment::Center))
        .padding([14, 18])
        .width(Length::Fill)
        .style(card_style)
        .into()
}

/// [`pane_header`] whose trailing slot is a plain count/status label.
fn pane_header_count<'a>(
    icon: &'static str,
    title: impl text::IntoFragment<'a>,
    subtitle: impl text::IntoFragment<'a>,
    trailing: impl text::IntoFragment<'a>,
) -> Element<'a, Message> {
    pane_header(
        icon,
        title,
        subtitle,
        text(trailing).size(13).style(muted).into(),
    )
}

/// A muted, indented note for an intentionally empty list.
fn empty_note(message: &'static str) -> Element<'static, Message> {
    container(text(message).size(13).style(muted))
        .padding([10, 14])
        .into()
}

/// A validated text input bound to a field's draft.
fn text_field(
    value: &str,
    path: &str,
    slot: Slot,
    has_error: bool,
    width: Length,
    placeholder: &str,
) -> Element<'static, Message> {
    let p = path.to_string();
    text_input(placeholder, value)
        .on_input(move |s| Message::Edit(EditAction::EditText(p.clone(), slot.clone(), s)))
        .padding([6, 8])
        .size(14)
        .width(width)
        .style(move |theme: &Theme, status| input_style(theme, status, has_error))
        .into()
}

// ---------------------------------------------------------------------------
// color picker popup
// ---------------------------------------------------------------------------

/// The floating color-picker panel: a saturation/value square, a hue strip, a
/// live preview, and synced HEX + R/G/B/A inputs. Every control edits the model
/// in real time (and live-applies to Hyprland when enabled).
fn color_picker_panel(app: &App, draft: &color_picker::ColorDraft) -> Element<'static, Message> {
    let target = draft.target.clone();
    let path = target.path().to_string();
    let hex_slot = match &target {
        color_picker::ColorTarget::Option(_) => Slot::Hex,
        color_picker::ColorTarget::Stop { index, .. } => Slot::Stop(*index),
    };
    let loaded = app.load.loaded();

    let color = loaded
        .and_then(|l| l.config.get(&path))
        .and_then(|v| match &target {
            color_picker::ColorTarget::Option(_) => value_to_color(v),
            color_picker::ColorTarget::Stop { index, .. } => value_stop(v, *index),
        })
        .unwrap_or_else(|| {
            HyprColor::from_hsv(
                f64::from(draft.hue),
                f64::from(draft.sat),
                f64::from(draft.val),
                255,
            )
        });

    let hex_draft = loaded
        .and_then(|l| l.draft(&path, hex_slot.clone()))
        .map(str::to_string)
        .unwrap_or_else(|| color.to_rgba_string());
    let hex_err = loaded
        .map(|l| l.field_error(&path, hex_slot.clone()).is_some())
        .unwrap_or(false);

    let area = canvas(color_picker::SvSquare {
        hue: draft.hue,
        sat: draft.sat,
        val: draft.val,
    })
    .width(Length::Fixed(232.0))
    .height(Length::Fixed(180.0));

    let hue = canvas(color_picker::HueStrip { hue: draft.hue })
        .width(Length::Fixed(24.0))
        .height(Length::Fixed(180.0));

    let fill = Color::from_rgba8(color.r, color.g, color.b, f32::from(color.a) / 255.0);
    let preview = container(Space::new().width(Length::Fill).height(Length::Fixed(40.0))).style(
        move |theme: &Theme| container::Style {
            background: Some(fill.into()),
            border: Border {
                color: theme.extended_palette().background.strong.color,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        },
    );

    let controls = column![
        preview,
        text_field(
            &hex_draft,
            &path,
            hex_slot,
            hex_err,
            Length::Fill,
            "rgba(rrggbbaa)"
        ),
        popup_channel_slider(&target, color, ColorChannel::R, "R", color.r),
        popup_channel_slider(&target, color, ColorChannel::G, "G", color.g),
        popup_channel_slider(&target, color, ColorChannel::B, "B", color.b),
        popup_channel_slider(&target, color, ColorChannel::A, "A", color.a),
    ]
    .spacing(8)
    .width(Length::Fixed(230.0));

    let header = row![
        text("Color picker").size(16).font(BOLD),
        Space::new().width(Length::Fill),
        button(text("✕").size(14))
            .padding([2, 8])
            .on_press(Message::CloseColorPicker)
            .style(ghost_button),
    ]
    .align_y(Alignment::Center);

    let footer = row![
        text(path).size(11).font(MONO).style(muted),
        Space::new().width(Length::Fill),
        button(text("Done").size(13))
            .padding([6, 16])
            .on_press(Message::CloseColorPicker)
            .style(changes_button_style),
    ]
    .align_y(Alignment::Center);

    container(column![header, row![area, hue, controls].spacing(14), footer,].spacing(14))
        .padding(18)
        .max_width(540.0)
        .style(card_style)
        .into()
}

/// A color channel slider inside the popup. Unlike the inline `channel_slider`
/// (which targets a scalar color option), this builds a full-color edit so it
/// works for both scalar options and gradient stops.
fn popup_channel_slider(
    target: &color_picker::ColorTarget,
    color: HyprColor,
    channel: ColorChannel,
    label: &'static str,
    value: u8,
) -> Element<'static, Message> {
    let path = target.path().to_string();
    let index = match target {
        color_picker::ColorTarget::Stop { index, .. } => Some(*index),
        color_picker::ColorTarget::Option(_) => None,
    };
    row![
        text(label).size(12).style(muted).width(Length::Fixed(14.0)),
        slider(0.0..=255.0, f64::from(value), move |v| {
            let mut c = color;
            let v = v.round() as u8;
            match channel {
                ColorChannel::R => c.r = v,
                ColorChannel::G => c.g = v,
                ColorChannel::B => c.b = v,
                ColorChannel::A => c.a = v,
            }
            let action = match index {
                Some(i) => EditAction::SetStopColor(path.clone(), i, c),
                None => EditAction::SetColor(path.clone(), c),
            };
            Message::Edit(action)
        })
        .step(1.0)
        .width(Length::Fill),
        text(value.to_string()).size(12).width(Length::Fixed(32.0)),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

/// Extract a [`HyprColor`] from a [`Value::Color`].
fn value_to_color(value: &Value) -> Option<HyprColor> {
    match value {
        Value::Color(c) => Some(*c),
        _ => None,
    }
}

/// Extract one stop from a [`Value::Gradient`].
fn value_stop(value: &Value, index: usize) -> Option<HyprColor> {
    match value {
        Value::Gradient(g) => g.stops.get(index).copied(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// pending changes view
// ---------------------------------------------------------------------------

fn changes_view(app: &App, loaded: &Loaded) -> Element<'static, Message> {
    let diff = loaded.pending_diff();
    let touched = loaded.touched_collections();
    let total = diff.len() + touched.len();
    let trailing = format!("{total} change{}", if total == 1 { "" } else { "s" });

    let mut items: Vec<Element<Message>> = vec![
        pane_header_count(
            "✎",
            "Pending changes",
            "Unsaved edits relative to the loaded file.",
            trailing,
        ),
        Space::new().height(Length::Fixed(4.0)).into(),
    ];

    if total == 0 {
        items.push(
            container(text("No unsaved changes.").size(14).style(muted))
                .padding([10, 14])
                .into(),
        );
    }

    for id in touched {
        let label = app
            .schema
            .collection(id)
            .map(|c| c.label.clone())
            .unwrap_or_default();
        let count = collection_count(app, id);
        items.push(
            container(
                row![
                    text(format!("{} {}", collection_icon(id), label)).size(14),
                    Space::new().width(Length::Fill),
                    text(format!("edited · {count} entries"))
                        .size(12)
                        .style(accent),
                ]
                .align_y(Alignment::Center),
            )
            .padding([10, 14])
            .width(Length::Fill)
            .style(card_style)
            .into(),
        );
    }

    for (path, old, new) in diff {
        let reset_path = path.clone();
        let row = row![
            column![
                text(path).size(14),
                row![
                    text(old).size(12).style(muted),
                    text("→").size(12).style(muted),
                    text(new).size(12).style(accent),
                ]
                .spacing(8),
            ]
            .spacing(3)
            .width(Length::Fill),
            tooltip(
                button(text("↶ revert").size(12))
                    .padding([3, 8])
                    .on_press(Message::Edit(EditAction::Revert(reset_path)))
                    .style(ghost_button),
                container(text("Back to the value in the file").size(12))
                    .padding([6, 10])
                    .style(tooltip_style),
                tooltip::Position::Left,
            ),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        items.push(
            container(row)
                .padding([10, 14])
                .width(Length::Fill)
                .style(card_style)
                .into(),
        );
    }

    scroll(items)
}

// ---------------------------------------------------------------------------
// diagnostics (issues found while loading)
// ---------------------------------------------------------------------------

/// The list of non-fatal issues found while reading the config, each explained
/// in plain language with its location and a hint on what to do.
fn diagnostics_view(loaded: &Loaded) -> Element<'static, Message> {
    let diagnostics = &loaded.diagnostics;
    let n = diagnostics.len();
    let trailing = format!("{n} item{}", if n == 1 { "" } else { "s" });

    let mut items: Vec<Element<Message>> = vec![
        pane_header_count(
            "⚠",
            "Diagnostics",
            "Issues found while reading your configuration. Nothing was lost — every line on \
             disk is preserved untouched.",
            trailing,
        ),
        Space::new().height(Length::Fixed(4.0)).into(),
    ];

    if diagnostics.is_empty() {
        items.push(
            container(
                text("✓ No problems — your configuration was understood completely.")
                    .size(14)
                    .style(success),
            )
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style)
            .into(),
        );
        return scroll(items);
    }

    for diagnostic in diagnostics {
        items.push(diagnostic_card(diagnostic));
    }

    scroll(items)
}

/// One diagnostic rendered as a card: a severity marker and message, then its
/// source location and a fix hint when known.
fn diagnostic_card(diagnostic: &Diagnostic) -> Element<'static, Message> {
    let icon = severity_icon(diagnostic.severity);
    let icon_style = severity_style(diagnostic.severity);

    let mut col = column![row![
        text(icon).size(14).style(icon_style),
        text(diagnostic.message.clone()).size(14),
    ]
    .spacing(8)
    .align_y(Alignment::Center)]
    .spacing(6);

    if let Some(location) = &diagnostic.location {
        col = col.push(
            text(format!("at {location}"))
                .size(12)
                .font(MONO)
                .style(muted),
        );
    }
    if let Some(hint) = &diagnostic.hint {
        col = col.push(text(format!("→ {hint}")).size(12).style(muted));
    }

    container(col)
        .padding([12, 16])
        .width(Length::Fill)
        .style(card_style)
        .into()
}

// ---------------------------------------------------------------------------
// profiles & recent files
// ---------------------------------------------------------------------------

fn profiles_view(app: &App) -> Element<'_, Message> {
    let mut items: Vec<Element<Message>> = vec![pane_header_count(
        "🗂",
        "Profiles & recents",
        "Save the current config as a named profile, reopen recents, or import any file.",
        "",
    )];

    // Save-as card (only meaningful with a loaded config).
    if app.load.loaded().is_some() {
        let mut save_btn = button(text("save profile").size(13))
            .padding([6, 12])
            .style(changes_button_style);
        if !app.profile_name.trim().is_empty() {
            save_btn = save_btn.on_press(Message::SaveProfile);
        }
        let mut card = column![
            text("Save current as profile").size(14).font(BOLD),
            row![
                text_input("profile name", &app.profile_name)
                    .on_input(Message::ProfileNameChanged)
                    .padding([6, 8])
                    .size(14)
                    .width(Length::Fill),
                save_btn,
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8);
        if let Some(status) = &app.save_status {
            match status {
                Ok(msg) => card = card.push(text(format!("✓ {msg}")).size(12).style(success)),
                Err(msg) => card = card.push(text(format!("✕ {msg}")).size(12).style(danger)),
            }
        }
        items.push(
            container(card)
                .padding([12, 16])
                .width(Length::Fill)
                .style(card_style)
                .into(),
        );
    }

    // Saved profiles. The list is read in `update` when this panel opens —
    // never here: a `read_dir` on the render path is filesystem I/O per frame.
    let mut saved = column![text("Saved profiles").size(14).font(BOLD)].spacing(8);
    if app.profiles.is_empty() {
        saved = saved.push(text("No saved profiles yet.").size(12).style(muted));
    }
    for profile in &app.profiles {
        saved = saved.push(
            row![
                text(&profile.name).size(14).width(Length::Fill),
                button(text("open").size(12))
                    .padding([3, 10])
                    .on_press(Message::OpenPath(profile.path.clone()))
                    .style(ghost_button),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    items.push(
        container(saved)
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style)
            .into(),
    );

    // Recent files.
    let mut recents = column![text("Recent files").size(14).font(BOLD)].spacing(6);
    if app.settings.recent_files.is_empty() {
        recents = recents.push(text("No recent files.").size(12).style(muted));
    }
    for recent in &app.settings.recent_files {
        recents = recents.push(
            button(text(recent.clone()).size(13).font(MONO))
                .width(Length::Fill)
                .padding([6, 10])
                .on_press(Message::OpenPath(std::path::PathBuf::from(recent)))
                .style(result_style),
        );
    }
    items.push(
        container(recents)
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style)
            .into(),
    );

    // Import from an arbitrary path.
    let trimmed = app.import_path.trim().to_string();
    let mut import_btn = button(text("load").size(13))
        .padding([6, 12])
        .style(ghost_button);
    if !trimmed.is_empty() {
        import_btn = import_btn.on_press(Message::OpenPath(std::path::PathBuf::from(trimmed)));
    }
    let import = column![
        text("Import from path").size(14).font(BOLD),
        row![
            text_input("/path/to/hyprland.conf or config.lua", &app.import_path)
                .on_input(Message::ImportPathChanged)
                .padding([6, 8])
                .size(14)
                .width(Length::Fill),
            import_btn,
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(8);
    items.push(
        container(import)
            .padding([12, 16])
            .width(Length::Fill)
            .style(card_style)
            .into(),
    );

    scroll(items)
}

// ---------------------------------------------------------------------------
// save panel
// ---------------------------------------------------------------------------

fn save_view(app: &App, loaded: &Loaded, preview: &save::SavePreview) -> Element<'static, Message> {
    let plan = &preview.plan;
    let problems = &preview.problems;
    let block_reason = save::blocked(problems, app.override_warnings);

    let mode_label = match plan.mode {
        SaveMode::Preserve => "preserve (edit in place)",
        SaveMode::Regenerate => "regenerate (fresh file)",
    };

    let subtitle = if loaded.is_multi_file() {
        format!("Mode: {mode_label}. Spans multiple files — only changed files are written.")
    } else {
        format!("Mode: {mode_label}. Review the diff, then write.")
    };
    let mut items: Vec<Element<Message>> = vec![pane_header_count(
        "💾",
        "Save",
        subtitle,
        format!("{} change(s)", plan.changed_files().len()),
    )];

    // Output format selector.
    items.push(
        container(
            row![
                text("Output format").size(13).style(muted),
                format_chip(plan.format, ConfigFormat::Conf, "conf"),
                format_chip(plan.format, ConfigFormat::Lua, "Lua"),
                Space::new().width(Length::Fill),
                text(if plan.format == loaded.format {
                    String::new()
                } else {
                    format!("converting from {}", format_label(loaded.format))
                })
                .size(12)
                .style(muted),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([10, 14])
        .width(Length::Fill)
        .style(card_style)
        .into(),
    );

    // Dynamic-Lua loss warning.
    if plan.drops_dynamic > 0 {
        items.push(
            container(
                text(format!(
                    "⚠ {} dynamic Lua region(s) (loops, functions, …) cannot be represented and will be dropped.",
                    plan.drops_dynamic
                ))
                .size(13)
                .style(warn_style),
            )
            .padding([10, 14])
            .width(Length::Fill)
            .style(card_style)
            .into(),
        );
    }

    // Validation results.
    items.push(validation_panel(problems));

    // Override + write controls.
    let mut controls = row![].spacing(12).align_y(Alignment::Center);
    if problems.iter().any(|p| p.severity == Severity::Warning) {
        controls = controls.push(
            iced::widget::checkbox(app.override_warnings)
                .label("save anyway (ignore warnings)")
                .on_toggle(Message::ToggleOverride)
                .size(16)
                .text_size(13),
        );
    }
    controls = controls.push(Space::new().width(Length::Fill));
    if let Some(reason) = &block_reason {
        controls = controls.push(text(reason.clone()).size(12).style(danger));
    }
    let mut write = button(text("⤓ write to disk").size(14))
        .padding([8, 16])
        .style(changes_button_style);
    if block_reason.is_none() && plan.has_changes() {
        write = write.on_press(Message::PerformSave);
    }
    controls = controls.push(write);
    items.push(
        container(controls)
            .padding([6, 14])
            .width(Length::Fill)
            .into(),
    );

    if !plan.has_changes() {
        items.push(
            container(
                text("Nothing to write — the model matches what's on disk.")
                    .size(13)
                    .style(muted),
            )
            .padding([10, 14])
            .into(),
        );
    }

    // Per-file diff/preview (precomputed in `update`, not here).
    for file_diff_data in &preview.diffs {
        items.push(file_diff(file_diff_data));
    }

    scroll(items)
}

fn format_chip(
    active: ConfigFormat,
    value: ConfigFormat,
    label: &'static str,
) -> Element<'static, Message> {
    chip(
        label.to_string(),
        active == value,
        Message::SetOutputFormat(value),
    )
}

fn validation_panel(problems: &[save::Problem]) -> Element<'static, Message> {
    if problems.is_empty() {
        return container(text("✓ No problems found.").size(13).style(success))
            .padding([10, 14])
            .width(Length::Fill)
            .style(card_style)
            .into();
    }

    let mut rows: Vec<Element<Message>> = vec![text("Validation").size(14).font(BOLD).into()];
    for problem in problems {
        let mark = severity_icon(problem.severity);
        let mark_style = severity_style(problem.severity);
        let mut label_btn = button(text(problem.label.clone()).size(13))
            .padding([2, 6])
            .style(ghost_button);
        if let Some(jump) = &problem.jump {
            label_btn = label_btn.on_press(Message::Selected(jump.clone()));
        }
        rows.push(
            row![
                text(mark).size(13).style(mark_style),
                label_btn,
                text(problem.message.clone()).size(12).style(muted),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
        );
    }

    container(Column::with_children(rows).spacing(6))
        .padding([12, 14])
        .width(Length::Fill)
        .style(card_style)
        .into()
}

fn file_diff(file: &save::FileDiff) -> Element<'static, Message> {
    let header = row![
        text(file.path.display().to_string()).size(13).font(BOLD),
        Space::new().width(Length::Fill),
        text(format!("+{}", file.added)).size(12).style(success),
        text(format!("-{}", file.removed)).size(12).style(danger),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let mut lines: Vec<Element<Message>> = Vec::new();
    for d in &file.lines {
        let (prefix, style): (&str, fn(&Theme) -> text::Style) = match d.tag {
            Tag::Insert => ("+", success),
            Tag::Delete => ("-", danger),
            Tag::Equal => (" ", muted),
        };
        lines.push(
            text(format!("{prefix} {}", d.text))
                .size(12)
                .font(MONO)
                .style(style)
                .into(),
        );
    }

    container(
        column![
            header,
            container(Column::with_children(lines).spacing(0))
                .padding([8, 10])
                .width(Length::Fill)
                .style(code_style),
        ]
        .spacing(8),
    )
    .padding([12, 14])
    .width(Length::Fill)
    .style(card_style)
    .into()
}

/// A vertical, scrollable, fill-width column.
///
/// Generic over the element lifetime so panes can hand it widgets that *borrow*
/// from the schema or the loaded config instead of cloning every string they
/// display into `'static` on each frame.
pub(super) fn scroll<'a>(items: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    scrollable(
        Column::with_children(items)
            .spacing(6)
            .width(Length::Fill)
            .padding([0, 8]),
    )
    .height(Length::Fill)
    .into()
}

// ---------------------------------------------------------------------------
// status bar
// ---------------------------------------------------------------------------

fn status_bar(app: &App) -> Element<'_, Message> {
    let content: Element<Message> = match &app.load {
        LoadState::Loading => text("Loading…").size(12).style(muted).into(),
        LoadState::NotFound { .. } => text("No configuration loaded").size(12).style(muted).into(),
        LoadState::Error { .. } => text("Error loading configuration")
            .size(12)
            .style(danger)
            .into(),
        LoadState::Loaded(loaded) => {
            let mut segs = row![
                container(text(format_label(loaded.format)).size(11).font(BOLD))
                    .padding([1, 8])
                    .style(format_badge_style),
                text(loaded.source.display().to_string())
                    .size(12)
                    .style(muted),
            ]
            .spacing(10)
            .align_y(Alignment::Center);

            if loaded.included_files > 0 {
                segs = segs.push(
                    text(format!("+{} included", loaded.included_files))
                        .size(12)
                        .style(muted),
                );
            }
            segs = segs.push(Space::new().width(Length::Fill));
            segs = segs.push(hyprland_status(app));
            // Counted in `update` when the config or the detected Hyprland
            // changes — scanning every set option against the schema on every
            // frame is work nobody asked for.
            if app.stale_options > 0 {
                segs = segs.push(
                    text(format!("⚠ {} need newer Hyprland", app.stale_options))
                        .size(12)
                        .style(warn_style),
                );
            }
            if let Some(status) = &app.save_status {
                // Clickable: a status line that never goes away becomes noise,
                // and the user has no other way to acknowledge it.
                let (label, style): (String, fn(&Theme) -> text::Style) = match status {
                    Ok(msg) => (format!("✓ {msg}"), success),
                    Err(msg) => (format!("✕ {msg}"), danger),
                };
                segs = segs.push(
                    button(text(label).size(12).style(style))
                        .padding([1, 8])
                        .on_press(Message::DismissStatus)
                        .style(ghost_button),
                );
            }
            segs = segs.push(
                text(format!("{} options set", loaded.config.option_count()))
                    .size(12)
                    .style(muted),
            );
            // Lua code hyprconf keeps as-is (loops, helper functions) is not a
            // problem and must not read like one; only real issues get the ⚠.
            let code = loaded.dynamic_regions;
            let issues = loaded.diagnostics.len().saturating_sub(code);
            if code > 0 {
                segs = segs.push(
                    button(
                        text(format!(
                            "{code} Lua code block{} kept as-is",
                            if code == 1 { "" } else { "s" }
                        ))
                        .size(12)
                        .style(muted),
                    )
                    .padding([1, 8])
                    .on_press(Message::ToggleDiagnostics)
                    .style(ghost_button),
                );
            }
            if issues > 0 {
                segs = segs.push(
                    button(
                        text(format!(
                            "⚠ {issues} warning{}",
                            if issues == 1 { "" } else { "s" }
                        ))
                        .size(12)
                        .style(warn_style),
                    )
                    .padding([1, 8])
                    .on_press(Message::ToggleDiagnostics)
                    .style(ghost_button),
                );
            }
            segs.into()
        }
    };

    container(content)
        .width(Length::Fill)
        .padding([6, 16])
        .style(bar_style)
        .into()
}

/// The Hyprland indicator: version badge + live-apply toggle + reload, or a
/// muted "not detected" note. Degrades gracefully when `hyprctl` is absent.
fn hyprland_status(app: &App) -> Element<'_, Message> {
    let Some(info) = &app.hyprland else {
        return text("Hyprland: not detected").size(12).style(muted).into();
    };

    let reload = button(text("⟳ reload").size(12))
        .padding([2, 8])
        .on_press(Message::Reload)
        .style(ghost_button);

    let mut strip = row![
        container(
            text(format!("Hyprland {}", info.version))
                .size(11)
                .font(BOLD)
        )
        .padding([1, 8])
        .style(format_badge_style),
        text("live").size(12).style(muted),
        toggler(app.live_apply)
            .on_toggle(Message::ToggleLiveApply)
            .size(16),
        reload,
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    if let Some(result) = &app.hypr_status {
        // A bare ✕ tells you nothing; the reason is one hover away.
        let (glyph, style, detail): (&str, fn(&Theme) -> text::Style, String) = match result {
            Ok(_) => ("✓", success, "Applied to the running Hyprland".to_string()),
            Err(e) => ("✕", danger, e.clone()),
        };
        strip = strip.push(tooltip(
            text(glyph).size(12).style(style),
            container(text(detail).size(12))
                .padding([6, 10])
                .max_width(420.0)
                .style(tooltip_style),
            tooltip::Position::Top,
        ));
    }
    strip.into()
}

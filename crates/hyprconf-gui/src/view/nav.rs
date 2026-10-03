// SPDX-License-Identifier: MIT OR Apache-2.0
//! The sidebar: every settings section and collection, grouped by the task it
//! serves rather than by how Hyprland happens to namespace its options.
//!
//! The old sidebar split the world into "sections" (scalar options) and
//! "collections" (lists) — an implementation detail. Someone tuning their
//! touchpad wants *Input*, *Devices* and *Gesture bindings* side by side, not
//! three screens apart in two different lists.

use iced::widget::{button, container, row, rule, scrollable, text, tooltip, Column};
use iced::{Alignment, Element, Length, Theme};

use hyprconf_core::schema::CollectionId;

use super::styles::*;
use super::BOLD;
use crate::{App, Message, Selection};

/// One sidebar entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NavItem {
    /// A schema section, by id.
    Section(&'static str),
    /// A structured collection.
    Collection(CollectionId),
}

use NavItem::{Collection as C, Section as S};

/// The sidebar layout. Every schema section and collection appears exactly
/// once (enforced by a test), so nothing can silently fall out of the UI.
pub(super) const NAV: &[(&str, &[NavItem])] = &[
    (
        "Look & feel",
        &[
            S("general"),
            S("decoration"),
            S("animations"),
            C(CollectionId::Animations),
            C(CollectionId::Beziers),
            S("group"),
        ],
    ),
    (
        "Layouts",
        &[S("layout"), S("dwindle"), S("master"), S("scrolling")],
    ),
    (
        "Input",
        &[
            S("input"),
            C(CollectionId::Devices),
            S("gestures"),
            C(CollectionId::Gestures),
            S("cursor"),
        ],
    ),
    (
        "Keyboard shortcuts",
        &[
            C(CollectionId::Keybinds),
            C(CollectionId::Submaps),
            S("binds"),
        ],
    ),
    (
        "Windows & workspaces",
        &[
            C(CollectionId::WindowRules),
            C(CollectionId::LayerRules),
            C(CollectionId::Workspaces),
        ],
    ),
    (
        "Displays",
        &[
            C(CollectionId::Monitors),
            S("render"),
            S("opengl"),
            S("xwayland"),
        ],
    ),
    (
        "Startup",
        &[
            C(CollectionId::Execs),
            C(CollectionId::Env),
            C(CollectionId::Variables),
            C(CollectionId::Plugins),
        ],
    ),
    (
        "System",
        &[
            S("misc"),
            S("ecosystem"),
            C(CollectionId::Permissions),
            S("input_capture"),
            S("quirks"),
            S("experimental"),
            S("debug"),
        ],
    ),
];

const SIDEBAR_WIDTH: f32 = 248.0;

/// Below this window width the sidebar collapses to icons-only.
///
/// Reads the *live* width, not the persisted one: `settings.window_width` is
/// only written a couple of times a second.
pub(super) fn is_compact(app: &App) -> bool {
    app.window_width < 860.0
}

pub(super) fn sidebar(app: &App) -> Element<'_, Message> {
    let compact = is_compact(app);
    // The highlight must track what the content pane is *actually* showing.
    let active = app.search.trim().is_empty()
        && !app.show_profiles
        && !app.show_save
        && !app.show_changes
        && !app.show_diagnostics
        && app.migration.is_none();
    let touched = app.load.loaded().map(|l| &l.touched);

    let mut items: Vec<Element<Message>> = Vec::new();
    for (gi, (group, entries)) in NAV.iter().enumerate() {
        if compact {
            if gi > 0 {
                items.push(
                    container(rule::horizontal(1).style(divider_style))
                        .padding([6, 8])
                        .into(),
                );
            }
        } else {
            items.push(group_header(group));
        }
        for item in *entries {
            let (selection, icon, label, badge) = match *item {
                NavItem::Section(id) => {
                    let Some((index, section)) = app
                        .schema
                        .sections()
                        .iter()
                        .enumerate()
                        .find(|(_, s)| s.id == id)
                    else {
                        continue;
                    };
                    // "Where did I change something?" is the question a
                    // settings tree gets asked, so sections count unsaved edits.
                    let dirty = app.dirty_by_section.get(index).copied().unwrap_or(0) as usize;
                    (
                        Selection::Section(section.id.clone()),
                        section_icon(id),
                        nav_label(&section.label),
                        (dirty > 0).then_some(Badge::Dirty(dirty)),
                    )
                }
                NavItem::Collection(id) => {
                    let Some(spec) = app.schema.collection(id) else {
                        continue;
                    };
                    let count = collection_count(app, id);
                    let badge = if touched.is_some_and(|t| t.contains(&id)) {
                        Some(Badge::Edited(count))
                    } else {
                        (count > 0).then_some(Badge::Count(count))
                    };
                    (
                        Selection::Collection(id),
                        collection_icon(id),
                        spec.label.as_str(),
                        badge,
                    )
                }
            };
            let selected = active && app.selected == selection;
            items.push(nav_button(icon, label, selection, selected, badge, compact));
        }
        if !compact {
            items.push(iced::widget::Space::new().height(Length::Fixed(6.0)).into());
        }
    }

    let list = Column::with_children(items)
        .spacing(2)
        .padding([10, if compact { 6 } else { 10 }])
        .width(Length::Fill);

    container(scrollable(list).height(Length::Fill))
        .width(Length::Fixed(if compact { 60.0 } else { SIDEBAR_WIDTH }))
        .height(Length::Fill)
        .style(panel_style)
        .into()
}

/// Section labels read better without a redundant "layout" suffix inside the
/// "Layouts" group ("Dwindle layout" → "Dwindle").
fn nav_label(label: &str) -> &str {
    label.strip_suffix(" layout").unwrap_or(label)
}

fn group_header(label: &'static str) -> Element<'static, Message> {
    container(text(label.to_uppercase()).size(10).font(BOLD).style(muted))
        .padding(iced::Padding {
            top: 8.0,
            right: 8.0,
            bottom: 4.0,
            left: 10.0,
        })
        .into()
}

/// What a sidebar entry's trailing badge means.
#[derive(Debug, Clone, Copy)]
enum Badge {
    /// How many entries a collection holds.
    Count(usize),
    /// How many options in a section have unsaved edits.
    Dirty(usize),
    /// A collection with unsaved edits (shown with its entry count).
    Edited(usize),
}

fn nav_button<'a>(
    icon: &'static str,
    label: &'a str,
    selection: Selection,
    selected: bool,
    badge: Option<Badge>,
    compact: bool,
) -> Element<'a, Message> {
    if compact {
        // Collapsed to an icon, a badge has nowhere to go — but "this has
        // unsaved edits" must not silently disappear, so it becomes a dot.
        let edited = matches!(badge, Some(Badge::Dirty(_) | Badge::Edited(_)));
        let glyph: Element<Message> = if edited {
            row![text(icon).size(16), text("●").size(8).style(accent)]
                .spacing(2)
                .align_y(iced::alignment::Vertical::Top)
                .into()
        } else {
            text(icon).size(16).into()
        };
        let b = button(container(glyph).center_x(Length::Fill))
            .width(Length::Fill)
            .padding([8, 0])
            .on_press(Message::Selected(selection))
            .style(move |theme: &Theme, status| nav_style(theme, status, selected));
        return tooltip(
            b,
            container(text(label).size(12))
                .padding([6, 10])
                .style(tooltip_style),
            tooltip::Position::Right,
        )
        .into();
    }

    let mut inner = row![
        container(text(icon).size(14)).width(Length::Fixed(20.0)),
        text(label).size(13).width(Length::Fill),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    match badge {
        Some(Badge::Count(n)) => inner = inner.push(count_badge(n, false)),
        Some(Badge::Dirty(n) | Badge::Edited(n)) => inner = inner.push(count_badge(n, true)),
        None => {}
    }

    button(inner)
        .width(Length::Fill)
        .padding([6, 10])
        .on_press(Message::Selected(selection))
        .style(move |theme: &Theme, status| nav_style(theme, status, selected))
        .into()
}

fn count_badge(count: usize, accent_tinted: bool) -> Element<'static, Message> {
    let style: fn(&Theme) -> container::Style = if accent_tinted {
        dirty_badge_style
    } else {
        badge_style
    };
    container(text(count.to_string()).size(10))
        .padding([1, 7])
        .style(style)
        .into()
}

/// How many entries a collection currently holds.
pub(super) fn collection_count(app: &App, id: CollectionId) -> usize {
    let Some(loaded) = app.load.loaded() else {
        return 0;
    };
    let c = &loaded.config;
    match id {
        CollectionId::Monitors => c.monitors.len(),
        CollectionId::Workspaces => c.workspaces.len(),
        CollectionId::WindowRules => c.window_rules.len(),
        CollectionId::LayerRules => c.layer_rules.len(),
        CollectionId::Keybinds => c.keybinds.len(),
        CollectionId::Submaps => c.submaps.len(),
        CollectionId::Env => c.env.len(),
        CollectionId::Execs => c.execs.len(),
        CollectionId::Variables => c.variables.len(),
        CollectionId::Beziers => c.beziers.len(),
        CollectionId::Animations => c.animations.len(),
        CollectionId::Gestures => c.gestures.len(),
        CollectionId::Devices => c.devices.len(),
        CollectionId::Permissions => c.permissions.len(),
        CollectionId::Plugins => c.plugins.len(),
    }
}

pub(super) fn section_icon(id: &str) -> &'static str {
    match id {
        "general" => "🪟",
        "decoration" => "🎨",
        "animations" => "✨",
        "input" => "⌨",
        "input_capture" => "📡",
        "gestures" => "✋",
        "group" => "🗂",
        "misc" => "🧩",
        "binds" => "🎹",
        "dwindle" => "🌿",
        "master" => "📐",
        "xwayland" => "🩹",
        "cursor" => "🖱",
        "render" => "🖼",
        "debug" => "🐞",
        "layout" => "🧱",
        "scrolling" => "📜",
        "opengl" => "🧊",
        "ecosystem" => "🌱",
        "experimental" => "🧪",
        "quirks" => "🔧",
        _ => "•",
    }
}

pub(super) fn collection_icon(id: CollectionId) -> &'static str {
    match id {
        CollectionId::Monitors => "🖥",
        CollectionId::Workspaces => "🔳",
        CollectionId::WindowRules => "📏",
        CollectionId::LayerRules => "🧅",
        CollectionId::Keybinds => "⌨",
        CollectionId::Submaps => "🗺",
        CollectionId::Env => "🌐",
        CollectionId::Execs => "▶",
        CollectionId::Variables => "🔣",
        CollectionId::Beziers => "〰",
        CollectionId::Animations => "🎞",
        CollectionId::Gestures => "👆",
        CollectionId::Devices => "🖲",
        CollectionId::Permissions => "🔐",
        CollectionId::Plugins => "🔌",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprconf_core::schema::Schema;

    /// Nothing can fall out of the UI: every section and collection the schema
    /// defines is reachable from the sidebar, exactly once.
    #[test]
    fn every_section_and_collection_is_in_the_sidebar_once() {
        let schema = Schema::shared();
        let items: Vec<NavItem> = NAV.iter().flat_map(|(_, i)| i.iter().copied()).collect();
        for section in schema.sections() {
            let n = items
                .iter()
                .filter(|i| matches!(i, NavItem::Section(id) if *id == section.id))
                .count();
            assert_eq!(n, 1, "section `{}` appears {n} times", section.id);
        }
        for collection in schema.collections() {
            let n = items
                .iter()
                .filter(|i| matches!(i, NavItem::Collection(id) if *id == collection.id))
                .count();
            assert_eq!(n, 1, "collection {:?} appears {n} times", collection.id);
        }
        assert_eq!(
            items.len(),
            schema.sections().len() + schema.collections().len()
        );
    }

    #[test]
    fn layout_suffix_is_dropped_in_the_sidebar() {
        assert_eq!(nav_label("Dwindle layout"), "Dwindle");
        assert_eq!(nav_label("Layout"), "Layout");
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! The drag-to-arrange monitor layout.
//!
//! A scale drawing of the desktop: one rectangle per display, sized by its
//! logical resolution and placed at its configured position. Dragging a
//! rectangle rewrites that display's `position` field, snapping to its
//! neighbours' edges so screens end up flush rather than a pixel apart.
//!
//! The canvas is a pure function of the tiles it is given — it holds no model
//! state and publishes only [`Message`]s.

use iced::mouse;
use iced::widget::canvas::{self, Event, Frame, Geometry, Path, Program, Stroke, Text};
use iced::widget::text::{Alignment as TextAlignment, LineHeight, Shaping};
use iced::{alignment, Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme, Vector};

use crate::edit::MonitorEdit;
use crate::Message;

/// Padding, in screen pixels, kept between the drawing and the canvas edge.
const PADDING: f32 = 14.0;

/// How close two edges must be, in *screen* pixels, to snap together. Expressed
/// on screen rather than in logical pixels so the feel is the same whether the
/// desktop is 1080p or spans four 4K panels.
const SNAP_SCREEN_PX: f32 = 8.0;

/// One display in the layout.
#[derive(Debug, Clone)]
pub struct Tile {
    /// Connector name, e.g. `DP-1`. Identifies the rule to edit.
    pub connector: String,
    /// 1-based number shown in the middle, matching the card list below.
    pub number: usize,
    /// Logical x position.
    pub x: f32,
    /// Logical y position.
    pub y: f32,
    /// Logical width (post-scale, post-rotation).
    pub width: f32,
    /// Logical height (post-scale, post-rotation).
    pub height: f32,
    /// Whether the display is on. Disabled ones are drawn hollow.
    pub enabled: bool,
    /// Whether this display currently holds focus.
    pub focused: bool,
    /// Whether the position comes from a real coordinate. `auto`-positioned
    /// displays are drawn where the compositor actually put them, but say so.
    pub auto: bool,
}

impl Tile {
    fn rect(&self) -> Rectangle {
        Rectangle {
            x: self.x,
            y: self.y,
            width: self.width.max(1.0),
            height: self.height.max(1.0),
        }
    }
}

/// The drag in progress: which tile, and where inside it the pointer grabbed.
#[derive(Debug, Default)]
pub struct DragState {
    /// Index into [`MonitorLayout::tiles`], if a drag is active.
    index: Option<usize>,
    /// Grab offset within the tile, in logical pixels.
    grab: Vector,
}

/// The layout program.
#[derive(Debug)]
pub struct MonitorLayout {
    /// Every display, in card order.
    pub tiles: Vec<Tile>,
}

impl MonitorLayout {
    /// The transform from logical desktop coordinates to canvas coordinates:
    /// a uniform scale plus an offset that centres the drawing.
    ///
    /// Returns `None` when there is nothing to draw.
    fn fit(&self, bounds: Size) -> Option<(f32, Vector)> {
        let (min_x, min_y, max_x, max_y) = self.extent()?;
        let (span_x, span_y) = ((max_x - min_x).max(1.0), (max_y - min_y).max(1.0));
        let usable_w = (bounds.width - PADDING * 2.0).max(1.0);
        let usable_h = (bounds.height - PADDING * 2.0).max(1.0);

        // Never magnify: a single small display centred at 1:1 reads better
        // than one blown up to fill the pane.
        let scale = (usable_w / span_x).min(usable_h / span_y).min(1.0);
        let offset = Vector::new(
            (bounds.width - span_x * scale) / 2.0 - min_x * scale,
            (bounds.height - span_y * scale) / 2.0 - min_y * scale,
        );
        Some((scale, offset))
    }

    /// The bounding box of every tile, in logical coordinates.
    fn extent(&self) -> Option<(f32, f32, f32, f32)> {
        let first = self.tiles.first()?;
        let mut bbox = (
            first.x,
            first.y,
            first.x + first.width,
            first.y + first.height,
        );
        for t in &self.tiles[1..] {
            bbox.0 = bbox.0.min(t.x);
            bbox.1 = bbox.1.min(t.y);
            bbox.2 = bbox.2.max(t.x + t.width);
            bbox.3 = bbox.3.max(t.y + t.height);
        }
        Some(bbox)
    }

    /// The topmost tile under a canvas-space point.
    ///
    /// Searched back to front so the tile drawn last (and therefore on top)
    /// wins an overlap — which is what the user sees and expects to grab.
    fn hit(&self, point: Point, scale: f32, offset: Vector) -> Option<usize> {
        let logical = Point::new((point.x - offset.x) / scale, (point.y - offset.y) / scale);
        self.tiles.iter().rposition(|t| t.rect().contains(logical))
    }
}

impl Program<Message> for MonitorLayout {
    type State = DragState;

    fn update(
        &self,
        state: &mut DragState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let (scale, offset) = self.fit(bounds.size())?;

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                let index = self.hit(p, scale, offset)?;
                let tile = &self.tiles[index];
                state.index = Some(index);
                state.grab = Vector::new(
                    (p.x - offset.x) / scale - tile.x,
                    (p.y - offset.y) / scale - tile.y,
                );
                // Capture so the press doesn't also reach anything beneath.
                Some(canvas::Action::request_redraw().and_capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let index = state.index?;
                let p = cursor.position_in(bounds)?;
                let tile = self.tiles.get(index)?;

                let raw = Point::new(
                    (p.x - offset.x) / scale - state.grab.x,
                    (p.y - offset.y) / scale - state.grab.y,
                );
                let (x, y) = snap(&self.tiles, index, raw, SNAP_SCREEN_PX / scale);

                Some(canvas::Action::publish(Message::MonitorEdit(
                    tile.connector.clone(),
                    MonitorEdit::Position(format!("{}x{}", x.round() as i64, y.round() as i64)),
                )))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let index = state.index.take()?;
                let connector = self.tiles.get(index)?.connector.clone();
                Some(canvas::Action::publish(Message::MonitorDropped(connector)))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &DragState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let Some((scale, offset)) = self.fit(bounds.size()) else {
            return vec![frame.into_geometry()];
        };

        let palette = theme.extended_palette();
        let accent = palette.primary.base.color;
        let hovered = cursor
            .position_in(bounds)
            .and_then(|p| self.hit(p, scale, offset));

        for (i, tile) in self.tiles.iter().enumerate() {
            let top_left = Point::new(tile.x * scale + offset.x, tile.y * scale + offset.y);
            let size = Size::new(tile.width * scale, tile.height * scale);
            // Rectangles land on adjacent logical pixels; a hairline inset keeps
            // their borders from merging into one thick line.
            let path = Path::rounded_rectangle(top_left, size, 4.0.into());

            let active = state.index == Some(i) || hovered == Some(i);
            let (fill, border, border_width) = if !tile.enabled {
                (
                    palette.background.weakest.color.scale_alpha(0.5),
                    palette.background.strong.color.scale_alpha(0.7),
                    1.0,
                )
            } else if active {
                (accent.scale_alpha(0.38), accent, 2.0)
            } else if tile.focused {
                (accent.scale_alpha(0.22), accent.scale_alpha(0.8), 1.5)
            } else {
                (
                    palette.background.strong.color.scale_alpha(0.45),
                    palette.background.strong.color,
                    1.0,
                )
            };

            frame.fill(&path, fill);
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(border)
                    .with_width(border_width),
            );

            // The number is the anchor that ties a rectangle to its card below,
            // so it gets the room; labels are dropped first when space is tight.
            let centre = Point::new(
                top_left.x + size.width / 2.0,
                top_left.y + size.height / 2.0,
            );
            let text_color =
                palette
                    .background
                    .base
                    .text
                    .scale_alpha(if tile.enabled { 0.9 } else { 0.45 });

            if size.height > 26.0 && size.width > 26.0 {
                frame.fill_text(label_text(
                    tile.number.to_string(),
                    Point::new(centre.x, centre.y - 6.0),
                    text_color,
                    22.0,
                ));
            }
            if size.height > 52.0 && size.width > 54.0 {
                frame.fill_text(label_text(
                    tile.connector.clone(),
                    Point::new(centre.x, centre.y + 16.0),
                    text_color.scale_alpha(0.75),
                    11.0,
                ));
            }
            if !tile.enabled && size.height > 78.0 && size.width > 60.0 {
                frame.fill_text(label_text(
                    "off".to_string(),
                    Point::new(centre.x, centre.y + 32.0),
                    text_color.scale_alpha(0.7),
                    10.0,
                ));
            } else if tile.auto && size.height > 78.0 && size.width > 60.0 {
                frame.fill_text(label_text(
                    "auto".to_string(),
                    Point::new(centre.x, centre.y + 32.0),
                    text_color.scale_alpha(0.6),
                    10.0,
                ));
            }
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &DragState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.index.is_some() {
            return mouse::Interaction::Grabbing;
        }
        let over = self
            .fit(bounds.size())
            .and_then(|(scale, offset)| {
                cursor
                    .position_in(bounds)
                    .and_then(|p| self.hit(p, scale, offset))
            })
            .is_some();
        if over {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::default()
        }
    }
}

fn label_text(content: String, position: Point, color: Color, size: f32) -> Text {
    Text {
        content,
        position,
        color,
        size: Pixels(size),
        line_height: LineHeight::Relative(1.0),
        font: Font::DEFAULT,
        align_x: TextAlignment::Center,
        align_y: alignment::Vertical::Center,
        shaping: Shaping::Basic,
        ..Text::default()
    }
}

/// Nudge a dragged position so its edges line up with its neighbours'.
///
/// Considers both flush placement (this display's right edge against another's
/// left) and alignment (both left edges equal), on each axis independently. The
/// nearest candidate within `threshold` wins; otherwise the raw position stands.
fn snap(tiles: &[Tile], index: usize, raw: Point, threshold: f32) -> (f32, f32) {
    let Some(moving) = tiles.get(index) else {
        return (raw.x, raw.y);
    };
    let (w, h) = (moving.width, moving.height);

    let mut best_x: Option<(f32, f32)> = None; // (distance, candidate)
    let mut best_y: Option<(f32, f32)> = None;

    let consider = |best: &mut Option<(f32, f32)>, candidate: f32, current: f32| {
        let distance = (candidate - current).abs();
        if distance <= threshold && best.is_none_or(|(d, _)| distance < d) {
            *best = Some((distance, candidate));
        }
    };

    for (i, other) in tiles.iter().enumerate() {
        if i == index {
            continue;
        }
        // Horizontal: sit flush left/right of it, or share an edge with it.
        for candidate in [
            other.x + other.width,
            other.x - w,
            other.x,
            other.x + other.width - w,
        ] {
            consider(&mut best_x, candidate, raw.x);
        }
        for candidate in [
            other.y + other.height,
            other.y - h,
            other.y,
            other.y + other.height - h,
        ] {
            consider(&mut best_y, candidate, raw.y);
        }
    }

    (
        best_x.map_or(raw.x, |(_, v)| v),
        best_y.map_or(raw.y, |(_, v)| v),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(number: usize, x: f32, y: f32, w: f32, h: f32) -> Tile {
        Tile {
            connector: format!("DP-{number}"),
            number,
            x,
            y,
            width: w,
            height: h,
            enabled: true,
            focused: false,
            auto: false,
        }
    }

    fn layout() -> MonitorLayout {
        MonitorLayout {
            tiles: vec![
                tile(1, 0.0, 0.0, 1920.0, 1080.0),
                tile(2, 1920.0, 0.0, 1280.0, 1024.0),
            ],
        }
    }

    #[test]
    fn the_drawing_is_centred_and_never_magnified() {
        let l = layout();
        // 3200x1080 of desktop into a 400x200 canvas: width-limited.
        let (scale, _) = l.fit(Size::new(400.0, 200.0)).unwrap();
        assert!((scale - (400.0 - PADDING * 2.0) / 3200.0).abs() < 1e-6);

        // A canvas far larger than the desktop must not blow it up.
        let (scale, offset) = l.fit(Size::new(8000.0, 8000.0)).unwrap();
        assert_eq!(scale, 1.0);
        assert_eq!(offset.x, (8000.0 - 3200.0) / 2.0, "centred horizontally");

        assert!(MonitorLayout { tiles: vec![] }
            .fit(Size::new(100.0, 100.0))
            .is_none());
    }

    #[test]
    fn hit_testing_prefers_the_tile_drawn_on_top() {
        let l = MonitorLayout {
            tiles: vec![
                tile(1, 0.0, 0.0, 100.0, 100.0),
                tile(2, 50.0, 50.0, 100.0, 100.0),
            ],
        };
        let (scale, offset) = (1.0, Vector::new(0.0, 0.0));
        assert_eq!(l.hit(Point::new(10.0, 10.0), scale, offset), Some(0));
        assert_eq!(l.hit(Point::new(140.0, 140.0), scale, offset), Some(1));
        // Inside both: the later (topmost) tile wins.
        assert_eq!(l.hit(Point::new(60.0, 60.0), scale, offset), Some(1));
        assert_eq!(l.hit(Point::new(400.0, 400.0), scale, offset), None);
    }

    #[test]
    fn dragging_snaps_flush_to_a_neighbour() {
        let l = layout();
        // Tile 2 dropped 5px short of tile 1's right edge snaps flush to it.
        let (x, y) = snap(&l.tiles, 1, Point::new(1915.0, 3.0), 8.0);
        assert_eq!((x, y), (1920.0, 0.0), "flush right, tops aligned");

        // Beyond the threshold the user's exact placement is respected.
        let (x, y) = snap(&l.tiles, 1, Point::new(1850.0, 400.0), 8.0);
        assert_eq!((x, y), (1850.0, 400.0));
    }

    #[test]
    fn snapping_also_aligns_far_edges() {
        let tiles = vec![
            tile(1, 0.0, 0.0, 1920.0, 1080.0),
            tile(2, 0.0, 0.0, 1920.0, 1080.0),
        ];
        // Stacked below, bottom-ish alignment: left edges snap together.
        let (x, _) = snap(&tiles, 1, Point::new(6.0, 1080.0), 8.0);
        assert_eq!(x, 0.0);

        // A narrower screen snapping its *right* edge to the other's right edge.
        let tiles = vec![
            tile(1, 0.0, 0.0, 1920.0, 1080.0),
            tile(2, 0.0, 0.0, 1280.0, 1024.0),
        ];
        let (x, _) = snap(&tiles, 1, Point::new(637.0, 1080.0), 8.0);
        assert_eq!(x, 640.0, "1920 - 1280");
    }

    #[test]
    fn a_lone_display_has_nothing_to_snap_to() {
        let tiles = vec![tile(1, 0.0, 0.0, 1920.0, 1080.0)];
        assert_eq!(snap(&tiles, 0, Point::new(37.0, 11.0), 8.0), (37.0, 11.0));
        assert_eq!(snap(&tiles, 9, Point::new(37.0, 11.0), 8.0), (37.0, 11.0));
    }
}

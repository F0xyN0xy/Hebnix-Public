use super::{Rgba, dcomp, gdi};
use crate::plugins::window_capture::CapturedFrame;
use std::cell::RefCell;
use std::sync::Arc;

enum Canvas {
    Gdi(windows::Win32::Graphics::Gdi::HDC),
    D2d(dcomp::D2dCanvas),
}

/// one recorded draw call. a frame is recorded first and only painted and
/// presented when it differs from the last one, so a static overlay costs dwm
/// nothing per tick, and an empty one hides the window so the game can go
/// back to flipping straight to the screen.
#[derive(PartialEq)]
enum Cmd {
    Line(f32, f32, f32, f32, Rgba, f32),
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Rgba,
        border: Rgba,
        width: f32,
        filled: bool,
        radius: f32,
    },
    Gradient(f32, f32, f32, f32, Rgba, Rgba, f32, f32),
    Circle(f32, f32, f32, Rgba, f32, bool),
    Text {
        x: f32,
        y: f32,
        s: String,
        color: Rgba,
        size: f32,
        halign: String,
        font: String,
        bold: bool,
        clip: Option<(f32, f32)>,
    },
    Polygon(Vec<(f32, f32)>, Rgba),
    Image(String, f32, f32, f32, f32, f32, f32),
    Capture {
        handle: u64,
        serial: u64,
        frame: Arc<CapturedFrame>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        opacity: f32,
    },
}

impl PartialEq for CapturedFrame {
    fn eq(&self, other: &Self) -> bool {
        self.serial == other.serial
    }
}

impl Cmd {
    /// gifs pick their frame from the wall clock, so they change without the
    /// recorded calls changing
    fn animates(&self) -> bool {
        matches!(self, Cmd::Image(path, ..) if path.to_ascii_lowercase().ends_with(".gif"))
    }

    /// false when an image couldn't be loaded yet, so the frame is incomplete
    fn paint(&self, canvas: &Canvas) -> bool {
        match (self, canvas) {
            (Cmd::Line(x1, y1, x2, y2, color, width), Canvas::Gdi(hdc)) => gdi::line(
                *hdc,
                *x1 as i32,
                *y1 as i32,
                *x2 as i32,
                *y2 as i32,
                *color,
                *width as i32,
            ),
            (Cmd::Line(x1, y1, x2, y2, color, width), Canvas::D2d(c)) => {
                c.line(*x1, *y1, *x2, *y2, *color, *width)
            }
            (
                Cmd::Rect {
                    x,
                    y,
                    w,
                    h,
                    fill,
                    border,
                    width,
                    filled,
                    radius,
                },
                canvas,
            ) => match canvas {
                Canvas::Gdi(hdc) => gdi::rect(
                    *hdc,
                    *x as i32,
                    *y as i32,
                    *w as i32,
                    *h as i32,
                    *fill,
                    *border,
                    *width as i32,
                    *filled,
                    *radius as i32,
                ),
                Canvas::D2d(c) => c.rect(*x, *y, *w, *h, *fill, *border, *width, *filled, *radius),
            },
            // gdi has no gradient, it fills with the first colour
            (Cmd::Gradient(x, y, w, h, c1, _, radius, _), Canvas::Gdi(hdc)) => gdi::rect(
                *hdc,
                *x as i32,
                *y as i32,
                *w as i32,
                *h as i32,
                *c1,
                *c1,
                0,
                true,
                *radius as i32,
            ),
            (Cmd::Gradient(x, y, w, h, c1, c2, radius, angle), Canvas::D2d(c)) => {
                c.gradient(*x, *y, *w, *h, *c1, *c2, *radius, *angle)
            }
            (Cmd::Circle(x, y, radius, color, width, filled), Canvas::Gdi(hdc)) => gdi::circle(
                *hdc,
                *x as i32,
                *y as i32,
                *radius as i32,
                *color,
                *width as i32,
                *filled,
            ),
            (Cmd::Circle(x, y, radius, color, width, filled), Canvas::D2d(c)) => {
                c.circle(*x, *y, *radius, *color, *width, *filled)
            }
            (
                Cmd::Text {
                    x,
                    y,
                    s,
                    color,
                    size,
                    halign,
                    font,
                    bold,
                    clip,
                },
                canvas,
            ) => match canvas {
                // gdi keeps its own font, no bold or clip
                Canvas::Gdi(hdc) => {
                    gdi::text(*hdc, *x as i32, *y as i32, s, *color, *size as i32, halign)
                }
                Canvas::D2d(c) => c.text(*x, *y, s, *color, *size, halign, font, *bold, *clip),
            },
            (Cmd::Polygon(points, color), Canvas::Gdi(hdc)) => {
                let pts: Vec<(i32, i32)> =
                    points.iter().map(|&(x, y)| (x as i32, y as i32)).collect();
                gdi::polygon(*hdc, &pts, *color);
            }
            (Cmd::Polygon(points, color), Canvas::D2d(c)) => c.polygon(points, *color),
            // gdi can't round, it just ignores the radius
            (Cmd::Image(path, x, y, w, h, opacity, _), Canvas::Gdi(hdc)) => {
                return gdi::image(
                    *hdc, path, *x as i32, *y as i32, *w as i32, *h as i32, *opacity,
                );
            }
            (Cmd::Image(path, x, y, w, h, opacity, radius), Canvas::D2d(c)) => {
                return c.image(path, *x, *y, *w, *h, *opacity, *radius);
            }
            (
                Cmd::Capture {
                    handle,
                    frame,
                    x,
                    y,
                    w,
                    h,
                    opacity,
                    ..
                },
                canvas,
            ) => match canvas {
                Canvas::Gdi(hdc) => gdi::capture_image(
                    *hdc, frame, *x as i32, *y as i32, *w as i32, *h as i32, *opacity,
                ),
                Canvas::D2d(c) => c.capture_image(*handle, frame, *x, *y, *w, *h, *opacity),
            },
        }
        true
    }
}

thread_local! {
    static RECORDING: RefCell<Option<Vec<Cmd>>> = const { RefCell::new(None) };
}

fn record(cmd: Cmd) {
    RECORDING.with(|r| {
        if let Some(cmds) = r.borrow_mut().as_mut() {
            cmds.push(cmd);
        }
    });
}

// Drawing primitives called from the Lua `draw` table. Recorded, then painted
// by frame(). No-ops outside a frame.

pub fn line(x1: f32, y1: f32, x2: f32, y2: f32, color: Rgba, width: f32) {
    record(Cmd::Line(x1, y1, x2, y2, color, width));
}

pub fn rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    fill: Rgba,
    border: Rgba,
    width: f32,
    filled: bool,
    radius: f32,
) {
    record(Cmd::Rect {
        x,
        y,
        w,
        h,
        fill,
        border,
        width,
        filled,
        radius,
    });
}

#[allow(clippy::too_many_arguments)]
pub fn gradient(x: f32, y: f32, w: f32, h: f32, c1: Rgba, c2: Rgba, radius: f32, angle: f32) {
    record(Cmd::Gradient(x, y, w, h, c1, c2, radius, angle));
}

pub fn circle(x: f32, y: f32, radius: f32, color: Rgba, width: f32, filled: bool) {
    record(Cmd::Circle(x, y, radius, color, width, filled));
}

#[allow(clippy::too_many_arguments)]
pub fn text(
    x: f32,
    y: f32,
    s: &str,
    color: Rgba,
    size: f32,
    halign: &str,
    font: &str,
    bold: bool,
    clip: Option<(f32, f32)>,
) {
    record(Cmd::Text {
        x,
        y,
        s: s.to_owned(),
        color,
        size,
        halign: halign.to_owned(),
        font: font.to_owned(),
        bold,
        clip,
    });
}

pub fn polygon(points: &[(f32, f32)], color: Rgba) {
    if points.len() < 3 {
        return;
    }
    record(Cmd::Polygon(points.to_vec(), color));
}

pub fn image(path: &str, x: f32, y: f32, w: f32, h: f32, opacity: f32, radius: f32) {
    record(Cmd::Image(path.to_owned(), x, y, w, h, opacity, radius));
}

pub fn capture_image(
    handle: u64,
    frame: Arc<CapturedFrame>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    opacity: f32,
) {
    record(Cmd::Capture {
        handle,
        serial: frame.serial,
        frame,
        x,
        y,
        w,
        h,
        opacity,
    });
}

/// the overlay window, whichever backend the machine supports
pub enum NativeOverlay {
    Dcomp(dcomp::DcompOverlay),
    Gdi(gdi::GdiOverlay),
}

/// what was last put on screen, to skip presenting identical frames
#[derive(Default)]
struct Shown {
    rect: Option<(i32, i32, i32, i32)>,
    cmds: Vec<Cmd>,
}

thread_local! {
    static SHOWN: RefCell<Shown> = RefCell::new(Shown::default());
}

impl NativeOverlay {
    pub fn new() -> Self {
        match dcomp::DcompOverlay::new() {
            Ok(o) => {
                tracing::info!("game overlay: DirectComposition backend");
                NativeOverlay::Dcomp(o)
            }
            Err(e) => {
                tracing::warn!("DirectComposition overlay unavailable ({e}); using GDI fallback");
                NativeOverlay::Gdi(gdi::GdiOverlay::new())
            }
        }
    }

    /// position over rect, let draw_fn record, then paint and present only if
    /// the frame changed. nothing drawn hides the window. draw_fn gets the
    /// overlay size in pixels.
    pub fn frame(&mut self, rect: (i32, i32, i32, i32), draw_fn: impl FnOnce(f32, f32)) {
        let (left, top, right, bottom) = rect;
        let (w, h) = ((right - left).max(1) as f32, (bottom - top).max(1) as f32);

        RECORDING.with(|r| *r.borrow_mut() = Some(Vec::new()));
        draw_fn(w, h);
        let cmds = RECORDING
            .with(|r| r.borrow_mut().take())
            .unwrap_or_default();

        if cmds.is_empty() {
            self.hide();
            return;
        }

        let unchanged = SHOWN.with(|s| {
            let s = s.borrow();
            s.rect == Some(rect) && s.cmds == cmds && !cmds.iter().any(Cmd::animates)
        });
        // the monitor thread can hide the window behind our back, so a frame
        // that didn't change still has to go up if the window isn't showing
        if unchanged && self.is_visible() {
            return;
        }

        let mut complete = true;
        let mut paint = |canvas: Canvas| {
            for cmd in &cmds {
                complete &= cmd.paint(&canvas);
            }
        };
        match self {
            NativeOverlay::Dcomp(o) => o.frame(rect, |canvas, _, _| paint(Canvas::D2d(canvas))),
            NativeOverlay::Gdi(o) => o.frame(rect, |hdc, _, _| paint(Canvas::Gdi(hdc))),
        }
        // a picture still loading (half-downloaded avatar) gets retried next tick
        let shown = if complete {
            Shown {
                rect: Some(rect),
                cmds,
            }
        } else {
            Shown::default()
        };
        SHOWN.with(|s| *s.borrow_mut() = shown);
    }

    fn is_visible(&self) -> bool {
        match self {
            NativeOverlay::Dcomp(o) => o.is_visible(),
            NativeOverlay::Gdi(o) => o.is_visible(),
        }
    }

    pub fn hide(&mut self) {
        match self {
            NativeOverlay::Dcomp(o) => o.hide(),
            NativeOverlay::Gdi(o) => o.hide(),
        }
        // force a repaint next time it shows
        SHOWN.with(|s| *s.borrow_mut() = Shown::default());
    }
}

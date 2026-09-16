//! What a frame costs to draw, on whatever machine runs it.
//!
//! Not a unit test — a measurement, run with `--ignored`, and it exists so the
//! figures in `README.md` and `docs/architecture.md` have a command behind
//! them rather than a memory:
//!
//! ```sh
//! cargo test --release --test render_bench -- --ignored --nocapture
//! ```
//!
//! **It stops at the `DrawTarget`.** What it measures is the deck turning a
//! `Screen` into pixels in memory. The frame then has to cross the Pi's single
//! USB 2.0 to the Pico — shared with the stick the window thread is reading
//! audio from — and *that* is the figure `architecture.md` calls unmeasured.
//! This one does not make it measured.
//!
//! Release only: a debug build is not the deck, and the numbers differ by
//! roughly an order of magnitude.

use deck_pi::browser::Row;
use deck_pi::display::paint::{Face, Layout, Painter, Palette};
use deck_pi::display::{compose, status_line, State};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use std::time::{Duration, Instant};

/// The panel as a plain buffer: no clipping cleverness, so what is timed is
/// the glyph blitting and not this.
struct Buf {
    w: u32,
    h: u32,
    px: Vec<BinaryColor>,
}

impl OriginDimensions for Buf {
    fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }
}

impl DrawTarget for Buf {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 && p.x < self.w as i32 && p.y < self.h as i32 {
                self.px[(p.y as u32 * self.w + p.x as u32) as usize] = c;
            }
        }
        Ok(())
    }
}

/// Plausible long-form material, mixed the way the real stick is.
const NAMES: &[&str] = &[
    "2024_録音",
    "追憶のウォーデンクリフ.aiff",
    "夜明けの前.wav",
    "雨音と遠雷_192k24.aiff",
    "第三楽章-長い名前で切り詰めが起きる例.wav",
    "ノイズフロア確認.aiff",
    "残響_88k2.wav",
    "序章.aiff",
    "終曲.wav",
    "test_44k1_16.wav",
    "ライブ音源",
    "圧縮済み.flac",
];

#[test]
#[ignore]
fn how_long_a_frame_takes_on_this_machine() {
    println!(
        "{:<14} {:>4} {:>5}  {:>12}  {:>12}",
        "panel", "cols", "rows", "full frame", "status only"
    );
    for (face, px_w, px_h, label) in [
        (Face::Px12, 128u32, 64u32, "128x64 @12"),
        (Face::Px12, 256, 64, "256x64 @12"),
        (Face::Px16, 320, 240, "320x240 @16"),
    ] {
        let p = Painter::new(face, Layout { px_w, px_h, colour: false });
        let g = p.geometry();
        let rows: Vec<Row> = (0..g.listing_rows())
            .map(|i| Row::Folder {
                name: NAMES[i % NAMES.len()].to_owned(),
                selected: i == 1,
            })
            .collect();
        let status = status_line(
            State::Playing,
            Some(44_100),
            Some(16),
            Some(Duration::from_secs(95)),
            g,
        );
        let screen = compose("録音", &rows, status.clone(), g);
        let pal = Palette::mono();
        let mut buf = Buf { w: px_w, h: px_h, px: vec![BinaryColor::Off; (px_w * px_h) as usize] };

        let n = 200;
        for _ in 0..20 {
            p.draw(&screen, &pal, &mut buf).unwrap();
        }
        let t0 = Instant::now();
        for _ in 0..n {
            p.draw(&screen, &pal, &mut buf).unwrap();
        }
        let full = t0.elapsed() / n;
        let t0 = Instant::now();
        for _ in 0..n {
            p.draw_status(&status, &pal, &mut buf).unwrap();
        }
        let status_only = t0.elapsed() / n;

        println!(
            "{label:<14} {:>4} {:>5}  {:>9.3} ms  {:>9.3} ms",
            g.cols,
            g.rows,
            full.as_secs_f64() * 1e3,
            status_only.as_secs_f64() * 1e3
        );
    }
    println!("\nDrawing into memory. The link to the Pico is not in these figures.");
}

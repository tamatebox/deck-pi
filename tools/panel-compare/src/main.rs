//! Renders the same browser screen on every candidate panel, at true physical
//! size, so the panel choice can be judged by looking rather than by arguing.
//! The analysis it feeds is GitHub issue #2.
//!
//! Two outputs:
//!
//! - `panel-compare.png` — every candidate on one sheet at 300 dpi, each drawn
//!   at its real millimetre size, with a 10 mm ruler. **Print at 100% and hold
//!   it at the distance a deck actually gets looked at.** A window on a Mac
//!   cannot answer a question about millimetres; a sheet printed at 100% can.
//! - `panel-<controller>-1x.png` — each panel at one image pixel per panel
//!   pixel, for inspecting the bitmap itself.
//!
//! The content is the browser as `architecture.md` describes it: the folder
//! tree *is* the index, so the screen is a path line, one folder's entries
//! with the selection on one of them, and a transport line. Folders and
//! unplayable files have to be distinguishable, and how that is paid for is
//! the interesting part — see `mark_cost`.

mod panels;
mod target;

use embedded_graphics::mono_font::{ascii::FONT_6X10, MonoTextStyle};
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use u8g2_fonts::{fonts, FontRenderer};

use panels::{candidates, Panel, Provenance};
use std::cell::RefCell;
use target::ImageTarget;

// Characters the font could not supply.
//
// The first version of this file wrote `let _ = font.render_aligned(...)`,
// which threw away exactly the information the harness exists to produce: a
// missing glyph came back as an error and the text silently vanished. The
// path line, the transport line and the truncation marker were all absent
// from the first render, and it took looking at the picture to notice. So
// failures are collected and reported now.
thread_local! {
    static MISSING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn note_failure(what: &str, text: &str) {
    MISSING.with(|m| {
        m.borrow_mut()
            .push(format!("{}: {:?}", what, text))
    });
}

/// The renders are generated, so they are not committed. The tool creates this
/// directory itself, and `.gitignore` excludes it.
const OUT_DIR: &str = "out";

const DPI: f64 = 300.0;
const PX_PER_MM: f64 = DPI / 25.4;

// A dark room, so everything is light on dark.
const BG: Rgb888 = Rgb888::new(0, 0, 0);
const FG: Rgb888 = Rgb888::new(255, 255, 255);
const DIM: Rgb888 = Rgb888::new(120, 120, 120);
/// Only reachable on a colour panel. This is the capability the mono
/// candidates cannot have at any size: marking a row without spending
/// characters on it.
const FOLDER: Rgb888 = Rgb888::new(120, 190, 255);
const REFUSED: Rgb888 = Rgb888::new(255, 90, 90);

#[derive(Clone, Copy)]
enum Kind {
    Folder,
    Playable,
    /// Refused on the header alone, before PLAY is pressed.
    Refused,
}

struct Entry {
    name: &'static str,
    kind: Kind,
}

/// One folder of plausible long-form material. Deliberately mixed: kanji,
/// kana, halfwidth digits, and one name long enough to force truncation on
/// every candidate, because how truncation looks is half the question.
fn listing() -> Vec<Entry> {
    use Kind::*;
    vec![
        Entry { name: "2024_録音", kind: Folder },
        Entry { name: "ライブ音源", kind: Folder },
        Entry { name: "夜明けの前.wav", kind: Playable },
        Entry { name: "雨音と遠雷_192k24.aiff", kind: Playable },
        Entry { name: "第三楽章-長い名前で切り詰めが起きる例.wav", kind: Playable },
        Entry { name: "圧縮済み.flac", kind: Refused },
        Entry { name: "32bit_float.wav", kind: Refused },
        Entry { name: "序章.aiff", kind: Playable },
        Entry { name: "残響_88k2.wav", kind: Playable },
        Entry { name: "終曲.wav", kind: Playable },
        Entry { name: "屋外採集/2023", kind: Folder },
        Entry { name: "test_44k1_16.wav", kind: Playable },
        Entry { name: "ノイズフロア確認.aiff", kind: Playable },
    ]
}

const SELECTED: usize = 3;

/// What distinguishing a row costs, in characters of the line.
///
/// On a mono panel a folder needs a trailing `/` and a refused file a leading
/// `!`, so the name loses one or two of the few cells it has. On a colour
/// panel the same distinction is a different pen and costs nothing — which is
/// the ILI9341's real advantage and is *not* the Gray4 argument. Gray4 is
/// wasted because it encodes intensity inside a glyph and a 1-bit glyph has
/// none; colour encodes which row is which, and that is free.
fn mark_cost(p: &Panel) -> u32 {
    if p.colour {
        0
    } else {
        1
    }
}

fn render_screen(p: &Panel, font: &FontRenderer, status_font: &FontRenderer) -> ImageTarget {
    let mut d = ImageTarget::new(p.px_w, p.px_h, BG);
    // One pixel of leading. Without it the rows touch — visible immediately
    // on the 8 px panel, where a full-width glyph fills its cell exactly.
    let row_h = p.row_h();
    let cell = p.glyph_px;
    let rows = p.browsable_rows();
    let entries = listing();

    // --- path line ---------------------------------------------------------
    // At 16 cells a deep path does not fit, which is itself a finding: the
    // browser has to show the current folder, not the whole path.
    let path = "../長尺/2024_録音";
    if font
        .render_aligned(
            path,
            Point::new(1, 0),
            VerticalPosition::Top,
            HorizontalAlignment::Left,
            FontColor::Transparent(DIM),
            &mut d,
        )
        .is_err()
    {
        note_failure(&format!("{} path line", p.name), path);
    }

    // --- entries -----------------------------------------------------------
    // Scrolled so the selection is visible, as the real browser would.
    let first = SELECTED.saturating_sub(rows as usize / 2);
    for i in 0..rows as usize {
        let Some(e) = entries.get(first + i) else { break };
        let y = (row_h * (i as u32 + 1)) as i32;
        let selected = first + i == SELECTED;

        let (mut pen, prefix, suffix) = match (e.kind, p.colour) {
            (Kind::Folder, true) => (FOLDER, "", ""),
            (Kind::Folder, false) => (FG, "", "/"),
            (Kind::Refused, true) => (REFUSED, "", ""),
            (Kind::Refused, false) => (DIM, "!", ""),
            (Kind::Playable, _) => (FG, "", ""),
        };

        if selected {
            let _ = Rectangle::new(Point::new(0, y), Size::new(p.px_w, row_h))
                .into_styled(PrimitiveStyle::with_fill(pen))
                .draw(&mut d);
            pen = BG;
        }

        // Truncate to what the line actually holds. Full-width cells, so one
        // character per cell; the mark eats into the same budget.
        let budget = (p.px_w / cell).saturating_sub(mark_cost(p)) as usize;
        let mut name: String = e.name.chars().take(budget).collect();
        if e.name.chars().count() > budget {
            name.pop();
            // ASCII, deliberately: U+2026 is not in these fonts, and a
            // truncation marker that silently fails to draw is worse than an
            // ugly one.
            name.push('~');
        }
        let line = format!("{}{}{}", prefix, name, suffix);

        if font
            .render_aligned(
                line.as_str(),
                Point::new(1, y),
                VerticalPosition::Top,
                HorizontalAlignment::Left,
                FontColor::Transparent(pen),
                &mut d,
            )
            .is_err()
        {
            note_failure(&format!("{} row {}", p.name, i), &line);
        }
    }

    // --- transport line ----------------------------------------------------
    let y = (p.px_h - p.glyph_px) as i32;
    let _ = Rectangle::new(Point::new(0, y - 1), Size::new(p.px_w, 1))
        .into_styled(PrimitiveStyle::with_fill(DIM))
        .draw(&mut d);
    // A play indicator is drawn, not typed: the arrow is not a glyph these
    // fonts carry, and finding that out by having the whole line disappear is
    // the reason failures are now reported.
    let t = (p.glyph_px as i32 * 2) / 3;
    for i in 0..t {
        let hh = (t - i).max(1) as u32;
        let _ = Rectangle::new(Point::new(1 + i, y + (t - hh as i32) / 2 + 1), Size::new(1, hh))
            .into_styled(PrimitiveStyle::with_fill(FG))
            .draw(&mut d);
    }
    let status = "192k/24  12:04 / 78:31";
    if status_font
        .render_aligned(
            status,
            Point::new(t + 3, y),
            VerticalPosition::Top,
            HorizontalAlignment::Left,
            FontColor::Transparent(FG),
            &mut d,
        )
        .is_err()
    {
        note_failure(&format!("{} transport line", p.name), status);
    }
    d
}

/// Nearest-neighbour scale to a true physical size. Not integer — the point is
/// the millimetres, not crisp pixels.
fn blit_true_size(sheet: &mut image::RgbImage, panel: &image::RgbImage, x0: u32, y0: u32, scale: f64) {
    let w = (panel.width() as f64 * scale).round() as u32;
    let h = (panel.height() as f64 * scale).round() as u32;
    for y in 0..h {
        for x in 0..w {
            let sx = ((x as f64 / scale) as u32).min(panel.width() - 1);
            let sy = ((y as f64 / scale) as u32).min(panel.height() - 1);
            if x0 + x < sheet.width() && y0 + y < sheet.height() {
                sheet.put_pixel(x0 + x, y0 + y, *panel.get_pixel(sx, sy));
            }
        }
    }
    // A hairline border so the panel's real extent is visible on paper.
    for x in 0..w {
        for y in [0u32, h.saturating_sub(1)] {
            if x0 + x < sheet.width() && y0 + y < sheet.height() {
                sheet.put_pixel(x0 + x, y0 + y, image::Rgb([90, 90, 90]));
            }
        }
    }
    for y in 0..h {
        for x in [0u32, w.saturating_sub(1)] {
            if x0 + x < sheet.width() && y0 + y < sheet.height() {
                sheet.put_pixel(x0 + x, y0 + y, image::Rgb([90, 90, 90]));
            }
        }
    }
}

fn main() {
    std::fs::create_dir_all(OUT_DIR).expect("create the output directory");

    let panels = candidates();
    if let Err(e) = panels::check_against_docs(&panels) {
        eprintln!("geometry disagrees with the design documents: {}", e);
        std::process::exit(1);
    }

    println!(
        "{:<16} {:>9} {:>8} {:>7} {:>7} {:>6} {:>5} {:>9} active area from",
        "panel", "px", "mm", "pitch", "glyph", "chars", "rows", "frame"
    );
    for p in &panels {
        println!(
            "{:<16} {:>4}x{:<4} {:>3.0}x{:<4.0} {:>6.3} {:>6.2} {:>6} {:>5} {:>6.1}ms {}  {}",
            p.name,
            p.px_w,
            p.px_h,
            p.mm_w,
            p.mm_h,
            p.pitch_mm(),
            p.glyph_mm(),
            p.chars_per_line() - mark_cost(p),
            p.browsable_rows(),
            p.frame_ms,
            p.bus,
            match p.provenance {
                Provenance::Diagonal => "nominal diagonal (assumed to be the active area)",
                Provenance::ModuleActiveArea => "published module active area",
            }
        );
    }
    println!(
        "\nchars is full-width Japanese cells after the mark a mono panel has to spend;\n\
         rows excludes the path line and the transport line."
    );

    // Fonts: 12x12 and 16x16 measured out of the u8g2 headers, japanese3 for
    // the widest kanji coverage.
    let f12 = FontRenderer::new::<fonts::u8g2_font_b12_t_japanese3>();
    let f16 = FontRenderer::new::<fonts::u8g2_font_b16_t_japanese3>();
    let f10 = FontRenderer::new::<fonts::u8g2_font_b10_t_japanese2>();
    // The status line is ASCII, so it takes a font that fits the row instead
    // of a 10 px kanji font crammed into a 9 px one — which is what made the
    // 8 px panel's rows collide in the first render.
    let a5 = FontRenderer::new::<fonts::u8g2_font_5x7_tr>();
    let a6 = FontRenderer::new::<fonts::u8g2_font_6x10_tr>();

    // Sheet big enough for the tallest column plus labels.
    // A4 landscape, with room to wrap. The first version laid every panel on
    // one row and ran two of them off the right edge.
    let sheet_w = (297.0 * PX_PER_MM) as u32;
    let sheet_h = (210.0 * PX_PER_MM) as u32;
    let sheet = image::RgbImage::from_pixel(sheet_w, sheet_h, image::Rgb([255, 255, 255]));
    let label = MonoTextStyle::new(&FONT_6X10, Rgb888::new(0, 0, 0));

    let mut x_mm = 10.0f64;
    let mut y_mm = 14.0f64;
    let mut row_tallest = 0.0f64;
    let mut label_target = ImageTarget { img: sheet };

    for p in &panels {
        // body draws names (kanji); status draws the transport line (ASCII).
        // The path line uses body, because a path has kanji in it.
        let (body, status): (&FontRenderer, &FontRenderer) = match p.glyph_px {
            16 => (&f16, &a6),
            12 => (&f12, &a6),
            _ => (&f10, &a5),
        };
        let screen = render_screen(p, body, status);

        // Each cell is the panel plus a label band; wrap when the row is full.
        let cell_w = p.mm_w.max(58.0) + 8.0;
        if x_mm + cell_w > 290.0 {
            x_mm = 10.0;
            y_mm += row_tallest + 22.0;
            row_tallest = 0.0;
        }
        row_tallest = row_tallest.max(p.mm_h);

        let _ = Text::new(
            p.controller,
            Point::new((x_mm * PX_PER_MM) as i32, (y_mm * PX_PER_MM) as i32),
            label,
        )
        .draw(&mut label_target);
        let detail = format!(
            "{:.0}x{:.0}mm  {}px = {:.2}mm  {}ch x {}rows",
            p.mm_w,
            p.mm_h,
            p.glyph_px,
            p.glyph_mm(),
            p.chars_per_line() - mark_cost(p),
            p.browsable_rows()
        );
        let _ = Text::new(
            &detail,
            Point::new((x_mm * PX_PER_MM) as i32, ((y_mm + 3.2) * PX_PER_MM) as i32),
            label,
        )
        .draw(&mut label_target);
        let detail2 = format!("{} {:.0}ms full frame", p.bus, p.frame_ms);
        let _ = Text::new(
            &detail2,
            Point::new((x_mm * PX_PER_MM) as i32, ((y_mm + 6.4) * PX_PER_MM) as i32),
            label,
        )
        .draw(&mut label_target);

        blit_true_size(
            &mut label_target.img,
            &screen.img,
            (x_mm * PX_PER_MM) as u32,
            ((y_mm + 9.0) * PX_PER_MM) as u32,
            p.pitch_mm() * PX_PER_MM,
        );

        let one_x = format!("{OUT_DIR}/panel-{}-1x.png", p.controller);
        screen.img.save(&one_x).expect("write 1x png");
        println!("wrote {}", one_x);

        x_mm += cell_w;
    }

    // A 10 mm ruler, so a print can be checked for 100% scale before trusting
    // anything about size.
    let ruler_y = (198.0 * PX_PER_MM) as u32;
    for i in 0..=10 {
        let x = ((10.0 + i as f64) * PX_PER_MM) as u32;
        let h = if i % 5 == 0 { 4.0 } else { 2.0 };
        for y in ruler_y..ruler_y + (h * PX_PER_MM) as u32 {
            if x < label_target.img.width() && y < label_target.img.height() {
                label_target.img.put_pixel(x, y, image::Rgb([0, 0, 0]));
            }
        }
    }
    let _ = Text::new(
        "10 mm - print at 100% and measure this before judging size",
        Point::new((10.0 * PX_PER_MM) as i32, (195.0 * PX_PER_MM) as i32),
        label,
    )
    .draw(&mut label_target);

    label_target
        .img
        .save(format!("{OUT_DIR}/panel-compare.png"))
        .expect("write sheet");
    println!("wrote {OUT_DIR}/panel-compare.png  ({} dpi, print at 100%)", DPI);

    MISSING.with(|m| {
        let m = m.borrow();
        if m.is_empty() {
            println!("\nevery string rendered — no missing glyphs");
        } else {
            println!(
                "\n{} strings the font could not draw. Anything here would have\n\
                 vanished from the screen with no error at runtime:",
                m.len()
            );
            for s in m.iter() {
                println!("  {}", s);
            }
        }
    });
}

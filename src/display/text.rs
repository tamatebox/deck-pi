//! A [`Screen`] as text, for a console.
//!
//! The panel is not bought and the packer that would carry pixels to the Pico
//! is not written, so this is what the deck shows today: the same `Screen`,
//! the same grid, the same marks, on a terminal over ssh. It is also the
//! fallback for a Pico that declares no panel, so it is not scaffolding that
//! gets deleted — it is one implementation of [`Show`](crate::app::panel::Show)
//! among the two or three there will be.
//!
//! **It renders the same decisions the panel will.** The marks come from
//! [`paint::marks`] rather than from a second copy of the rule, so a folder is
//! `/` and a refused file `!` here exactly as on a mono panel, and the
//! truncation is the model's. What a terminal cannot reproduce is the pixels;
//! what it reproduces exactly is the *layout*, which is the half that has been
//! got wrong before.
//!
//! # Redrawing in place, and what breaks it
//!
//! With ANSI enabled this walks the cursor back over the block it last wrote
//! and overwrites it, so a fast spin does not scroll a wall of listings past.
//! That arithmetic is only right while nothing else writes to the same
//! terminal, which is why [`Text::note`] exists: anything the deck wants to
//! say goes through it, and it drops the block first so the next draw starts
//! clean.
//!
//! Without ANSI — piped to a file, or a terminal that is not one — each draw
//! is simply appended. The caller decides which, because deciding it here
//! would mean an `isatty` call and `libc` is a Linux-only dependency of this
//! crate.

use std::io::Write;

use crate::app::panel::Show;
use crate::display::paint::marks;
use crate::display::{Geometry, Screen};

/// A grid wide enough to read a track name on, for a console that is not
/// pretending to be any particular panel.
pub const CONSOLE: Geometry = Geometry {
    cols: 40,
    rows: 12,
    colour: false,
};

pub struct Text<W: Write> {
    out: W,
    geometry: Geometry,
    ansi: bool,
    /// Lines written by the last draw, and the whole basis of the cursor
    /// arithmetic. Zero means "nothing of ours is on screen".
    on_screen: usize,
}

impl<W: Write> Text<W> {
    pub fn new(out: W, geometry: Geometry) -> Text<W> {
        Text {
            out,
            geometry,
            ansi: false,
            on_screen: 0,
        }
    }

    /// Overwrite in place rather than appending. For a terminal.
    pub fn with_ansi(mut self, ansi: bool) -> Text<W> {
        self.ansi = ansi;
        self
    }

    /// Says something that is not the screen — a medium arriving, a refused
    /// press — without leaving the cursor arithmetic wrong.
    pub fn note(&mut self, line: &str) -> std::io::Result<()> {
        self.rewind()?;
        writeln!(self.out, "{line}")?;
        self.out.flush()
    }

    /// Puts the cursor back where the last block started, and forgets it.
    fn rewind(&mut self) -> std::io::Result<()> {
        if self.ansi && self.on_screen > 0 {
            write!(self.out, "\x1b[{}A\x1b[0J", self.on_screen)?;
        }
        self.on_screen = 0;
        Ok(())
    }

    /// The block, at a constant height, which is what makes [`Self::rewind`]
    /// land where it should.
    fn block(&self, screen: &Screen) -> Vec<String> {
        let g = self.geometry;
        let mut out = Vec::with_capacity(g.rows);
        out.push(screen.folder.clone());
        for i in 0..g.listing_rows() {
            out.push(match screen.lines.get(i) {
                None => String::new(),
                Some(line) => {
                    let (prefix, suffix) = marks(line.kind, g.colour);
                    let text = format!("{prefix}{}{suffix}", line.text);
                    if !line.selected {
                        text
                    } else if self.ansi {
                        // Reverse video, which is what the panel's filled bar
                        // is and costs no column — the panel does not spend
                        // one on the selection either.
                        format!("\x1b[7m{text:<width$}\x1b[0m", width = g.cols)
                    } else {
                        // No way to invert, so the selection costs a column
                        // here that it does not cost on the panel. Marked as
                        // a difference rather than passed off as the layout.
                        format!(">{text}")
                    }
                }
            });
        }
        out.push(screen.status.clone());
        out
    }
}

impl<W: Write> Show for Text<W> {
    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn show(&mut self, screen: &Screen) -> std::io::Result<()> {
        self.rewind()?;
        let lines = self.block(screen);
        for line in &lines {
            writeln!(self.out, "{line}")?;
        }
        self.on_screen = lines.len();
        self.out.flush()
    }

    // **No `show_status` override, deliberately.** Overwriting one line in
    // place would need the cursor moved up and back down, and the saving is a
    // few hundred bytes on a terminal. `Redraw::Position` exists for a link
    // that costs something; this one does not have one.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{compose, Geometry, Kind, Line, Screen};

    fn screen() -> Screen {
        Screen {
            folder: "録音".into(),
            lines: vec![
                Line { text: "2024_録音".into(), kind: Kind::Folder, selected: false },
                Line { text: "夜明けの前.wav".into(), kind: Kind::Playable, selected: true },
                Line { text: "圧縮済み.flac".into(), kind: Kind::Refused, selected: false },
            ],
            status: "PLAY 1:35.0 44k/16".into(),
        }
    }

    fn render(g: Geometry, ansi: bool) -> String {
        let mut buf = Vec::new();
        {
            let mut t = Text::new(&mut buf, g).with_ansi(ansi);
            t.show(&screen()).expect("show");
        }
        String::from_utf8(buf).expect("utf-8")
    }

    #[test]
    fn the_block_is_always_the_panels_height() {
        // The cursor arithmetic in `rewind` is only right if every draw is
        // the same number of lines — a short listing has to be padded, not
        // shortened, or the next redraw walks up into whatever was above.
        for rows in [4, 6, 12] {
            let g = Geometry { rows, ..CONSOLE };
            let out = render(g, false);
            assert_eq!(out.lines().count(), rows, "at {rows} rows: {out:?}");
        }
    }

    #[test]
    fn it_shows_the_marks_the_mono_panel_shows() {
        // Same rule, from the same function — not a second copy that agrees
        // today. A folder takes `/`, a refused file `!`.
        let out = render(CONSOLE, false);
        assert!(out.contains("2024_録音/"), "{out}");
        assert!(out.contains("!圧縮済み.flac"), "{out}");
        assert!(out.starts_with("録音\n"), "{out}");
        assert!(out.trim_end().ends_with("PLAY 1:35.0 44k/16"), "{out}");
    }

    #[test]
    fn the_selection_costs_a_column_only_where_it_has_to() {
        // On a terminal that can invert, the bar is reverse video and the
        // name keeps every column the panel would give it. Without ANSI there
        // is no way to say it but a character, and that is a difference from
        // the panel rather than a rendering of it.
        let plain = render(CONSOLE, false);
        assert!(plain.contains(">夜明けの前.wav"), "{plain}");

        let ansi = render(CONSOLE, true);
        assert!(ansi.contains("\x1b[7m夜明けの前.wav"), "{ansi:?}");
        assert!(!ansi.contains(">夜明けの前.wav"), "{ansi:?}");
    }

    #[test]
    fn a_note_drops_the_block_so_the_next_draw_starts_clean() {
        // Without this the cursor walks up into the note and overwrites it,
        // and the deck's only way of saying anything is lost under the
        // listing.
        let mut buf = Vec::new();
        let mut t = Text::new(&mut buf, CONSOLE).with_ansi(true);
        t.show(&screen()).expect("show");
        t.note("medium: browsable").expect("note");
        t.show(&screen()).expect("show again");
        let out = String::from_utf8(buf).expect("utf-8");

        // Two rewinds: one before the note, one before the second draw.
        assert_eq!(out.matches("\x1b[12A").count(), 1, "{out:?}");
        assert!(out.contains("medium: browsable\n"), "{out:?}");
    }

    #[test]
    fn an_empty_deck_still_draws_a_block() {
        // The state a deck is in when it is switched on, and the one where a
        // display that only drew "when there is something" would show
        // nothing at all and look broken.
        let g = CONSOLE;
        let empty = Screen {
            folder: crate::app::panel::NO_MEDIUM.to_owned(),
            lines: Vec::new(),
            status: "NO TRACK".into(),
        };
        let mut buf = Vec::new();
        {
            let mut t = Text::new(&mut buf, g);
            t.show(&empty).expect("show");
        }
        let out = String::from_utf8(buf).expect("utf-8");
        assert_eq!(out.lines().count(), g.rows);
        assert!(out.starts_with("No USB\n"), "{out}");
        assert!(out.trim_end().ends_with("NO TRACK"), "{out}");
    }

    #[test]
    fn the_model_is_what_truncates_and_this_does_not_widen_it() {
        // The marks are added after `fit` has already taken them off the
        // budget, so a marked line is still one panel wide. A text display
        // that padded or re-cut here would be a second truncation rule.
        let g = Geometry { cols: 12, rows: 4, colour: false };
        let rows = [crate::browser::Row::Folder {
            name: "とても長いフォルダ名".into(),
            selected: false,
        }];
        let screen = compose("録音", &rows, "PAUSE".into(), g);
        let mut buf = Vec::new();
        {
            let mut t = Text::new(&mut buf, g);
            t.show(&screen).expect("show");
        }
        let out = String::from_utf8(buf).expect("utf-8");
        let listing = out.lines().nth(1).expect("a listing row");
        assert!(listing.ends_with('/'), "{listing:?}");
        assert_eq!(
            crate::display::width(listing),
            g.cols,
            "{listing:?} is not one panel wide"
        );
    }
}

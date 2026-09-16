//! What the screen says, decided in **cells** rather than in pixels.
//!
//! `architecture.md` keeps the panel model out of the UI code: the drawing goes
//! through `embedded-graphics`' `DrawTarget`, and the device constructor is the
//! only line that changes when the panel does. This module is the half that
//! sits above even that — it decides *what text goes where* given a grid of
//! cells, and it does so without a font, a driver or a panel.
//!
//! **Which is the half that needs no panel.** Which panel is still open
//! ([#2](https://github.com/tamatebox/deck-pi/issues/2)), and nothing here
//! depends on the answer. The other half — cells into pixels, against the
//! real faces — is [`paint`], and it takes the grid below from the font
//! rather than the other way round.
//!
//! The licence over those glyphs is
//! [#10](https://github.com/tamatebox/deck-pi/issues/10), and the trace lives
//! there rather than here — a licence note in a source file is how this one
//! got lost the first time. **One checkbox in it is open**, which is what
//! gates shipping rather than what gates writing the drawing.
//!
//! # A column is half a cell, and that is a font fact
//!
//! **An earlier version of this module said "one character, one cell" and
//! cited `panel-compare` for it.** The harness says the opposite, in
//! `panels.rs`: *"Full-width Japanese characters per line. Halfwidth ASCII
//! fits two per cell in these fonts, so a mixed name does better than this."*
//! Measured on the real faces, `a` is 6 px in `b12` where `弘` is 12. The
//! citation was shaped like a citation and was a recollection, which
//! `CLAUDE.md` says has cost this project real errors — this one truncated
//! every Latin name to half the panel.
//!
//! So the unit here is a **column**: one half-width glyph. A kanji is two, an
//! ASCII character is one, and `unicode-width` decides which by East Asian
//! Width rather than by a hand-kept table.
//!
//! **Neither is bytes**: `追憶` is six bytes, two characters and four columns,
//! and cutting on a byte boundary draws a replacement glyph — or nothing, on a
//! font without one.
//!
//! The model's arithmetic is a *plan*. Ambiguous-width characters exist, and
//! the renderer is the only thing that knows the truth: it should measure each
//! line against the font and clip, rather than trust a budget computed here.

pub mod paint;
pub mod packed;
pub mod text;
pub mod wire;

use std::time::Duration;

use unicode_width::UnicodeWidthChar;

use crate::browser::{Row, Verdict};
pub use crate::transport::State;

/// A truncation that fits, marked so it is visibly a truncation.
///
/// **ASCII deliberately.** `panel-compare` settled this: `U+2026` is not in
/// these fonts, and a truncation marker that silently fails to draw is worse
/// than an ugly one — the name simply looks like a different, shorter name.
pub const TRUNCATED: char = '~';

/// The panel's usable text grid, in **columns** of one half-width glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// `panel_px_wide / (glyph_px / 2)`.
    pub cols: usize,
    pub rows: usize,
    /// Whether the panel can say what a row *is* with colour.
    ///
    /// A mono panel spends a column on a `/` or a `!` instead, and that column
    /// comes out of the name's budget — which is why this is here and not in
    /// the renderer. A name truncated against the wrong budget is wrong in a
    /// way nobody notices until the panel is in a box.
    pub colour: bool,
}

impl Geometry {
    /// Rows left for the listing once the path line and the status line have
    /// taken theirs. Saturating: a two-row panel is absurd but must not panic.
    pub fn listing_rows(self) -> usize {
        self.rows.saturating_sub(2)
    }

    /// Columns the kind-mark costs: none on colour, one on mono.
    pub fn mark_cols(self) -> usize {
        usize::from(!self.colour)
    }

    /// Columns a name may use.
    pub fn name_budget(self) -> usize {
        self.cols.saturating_sub(self.mark_cols())
    }
}

/// How many columns a character occupies.
///
/// East Asian Wide and Fullwidth are two; everything else is one. Control
/// characters report `None` from the crate and are treated as zero, which is
/// right for a budget — they draw nothing.
pub fn cols_of(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// How many columns a string occupies.
pub fn width(text: &str) -> usize {
    text.chars().map(cols_of).sum()
}

/// What a row is, for the renderer to mark however the panel allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Playable,
    /// Decidable from the header and refused there — `decisions.md` shows
    /// these rather than hiding them, so a folder of FLAC does not look empty.
    Refused,
    /// libsndfile would not open it at all.
    Unreadable,
}

/// One line of the listing, already cut to fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub kind: Kind,
    pub selected: bool,
}

/// Everything the screen shows, and nothing about how it is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The current folder, not the whole path.
    ///
    /// `panel-compare` turned this up as a finding rather than a choice: at
    /// sixteen cells a deep path does not fit, so showing one is showing a
    /// truncated middle of it, which locates you nowhere.
    pub folder: String,
    pub lines: Vec<Line>,
    /// The transport, the position, and the rate and depth.
    ///
    /// **The rate and depth are not a verification**, though this file used to
    /// say they were. A panel showing `44k/16` reports what the deck believes;
    /// a deck wrong about its own output would print the wrong number with the
    /// same confidence. `--device=` is the check, because it reads `hw_params`
    /// back from `/proc/asound` — a different source. They are kept because
    /// they cost nothing on every candidate geometry, not because they confirm
    /// anything.
    pub status: String,
}

/// Cuts a name to a budget in cells, marking it if anything was lost.
///
/// The marker replaces a character rather than being added to the budget,
/// because a line that is one cell too long is not drawn short — it is drawn
/// over the edge of the panel, or wrapped, depending on the driver.
pub fn fit(name: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    if width(name) <= budget {
        return name.to_owned();
    }
    // Room for the marker, which costs one column.
    let room = budget - 1;
    let mut out = String::new();
    let mut used = 0;
    for c in name.chars() {
        let w = cols_of(c);
        if used + w > room {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push(TRUNCATED);
    out
}

fn kind_of(row: &Row) -> Kind {
    match row {
        Row::Folder { .. } => Kind::Folder,
        Row::File { verdict, .. } => match verdict {
            Verdict::Plays(_) => Kind::Playable,
            Verdict::Refused(_) => Kind::Refused,
            Verdict::Unreadable(_) => Kind::Unreadable,
        },
    }
}

/// Renders `mm:ss.d`, which is what the CDJ-350 shows and what a cue is set
/// against. Tenths rather than frames: a frame count is unreadable at a
/// glance and the cue store keeps the exact value anyway.
pub fn timecode(at: Duration) -> String {
    let tenths = at.as_millis() / 100;
    let (m, s, d) = (tenths / 600, (tenths / 10) % 60, tenths % 10);
    format!("{m}:{s:02}.{d}")
}

/// The status line: what is loaded, and what the chain is actually doing.
pub fn status_line(
    state: State,
    rate_hz: Option<u32>,
    bits: Option<u16>,
    at: Option<Duration>,
    g: Geometry,
) -> String {
    // **No STOP.** `transport.rs` is explicit that a CDJ has no such
    // gesture — returning to the cue and pausing is what stopping means, so
    // both land on `Paused`. `Stopped` means *nothing loaded* and only that,
    // and printing "STOP" for it would put a word on the panel that the
    // design says does not exist, for the one state it does not describe.
    // FF and REW are the button names README uses, and are shorter than SEEK.
    let word = match state {
        State::Stopped => "NO TRACK",
        State::Playing => "PLAY",
        State::Paused => "PAUSE",
        State::SeekingForward => "FF",
        State::SeekingBack => "REW",
    };
    let mut line = String::from(word);
    // An empty deck has no position and no rate to report; the fields below
    // would be a time into nothing.
    if state == State::Stopped {
        return fit(&line, g.cols);
    }
    if let Some(at) = at {
        line.push(' ');
        line.push_str(&timecode(at));
    }
    // **Dropped first when the panel is narrow, and that is the right order.**
    // The transport word and the position are what a hand reaches for mid-set;
    // the rate and depth are what you read once, when confirming the chain.
    if let (Some(hz), Some(bits)) = (rate_hz, bits) {
        let tail = format!(" {}k/{}", hz / 1000, bits);
        if width(&line) + width(&tail) <= g.cols {
            line.push_str(&tail);
        }
    }
    fit(&line, g.cols)
}

/// Builds the whole screen from a browser view and the transport's state.
///
/// Takes the rows rather than the `Browser` so this is callable with a handful
/// of literals in a test, which is most of why the split is here at all.
pub fn compose(folder: &str, rows: &[Row], status: String, g: Geometry) -> Screen {
    // The caller asks `Browser::view` for a height, and that height must be
    // this one. Handed more rows than fit, the truncation below is silent and
    // takes them off the **end** — which is where the selection is when the
    // user has scrolled down.
    debug_assert!(
        rows.len() <= g.listing_rows(),
        "compose was given {} rows for {} — call view(g.listing_rows())",
        rows.len(),
        g.listing_rows()
    );
    Screen {
        folder: fit(folder, g.cols),
        lines: rows
            .iter()
            .take(g.listing_rows())
            .map(|r| Line {
                text: fit(r.name(), g.name_budget()),
                kind: kind_of(r),
                selected: r.selected(),
            })
            .collect(),
        status,
    }
}

/// What a turn of the loop should redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redraw {
    Nothing,
    /// The position field only. Cheap, and the reason the rule below is not
    /// simply "on state change".
    Position,
    Full,
}

/// Coalescing window for state changes.
///
/// `architecture.md`: "Coalesce encoder events and redraw at most every
/// 30-50 ms, or fast scrolling falls behind." A detent is one `Browse(1)`, and
/// a fast spin is tens of them; redrawing each would spend the whole budget
/// drawing frames nobody sees, and the listing would lag the knob.
pub const COALESCE: Duration = Duration::from_millis(40);

/// How often the position advances while a track plays.
///
/// One second, per `architecture.md`, which also says why this exists at all:
/// a position readout has to move while nothing is changing, and that *is* a
/// timer even though the rule is "on state change".
pub const POSITION_EVERY: Duration = Duration::from_secs(1);

/// Decides when to draw, and nothing else.
#[derive(Debug, Clone)]
pub struct Cadence {
    last_full: Option<Duration>,
    last_position: Option<Duration>,
    pending: bool,
}

impl Default for Cadence {
    fn default() -> Self {
        Self::new()
    }
}

impl Cadence {
    pub fn new() -> Cadence {
        Cadence {
            last_full: None,
            last_position: None,
            pending: false,
        }
    }

    /// Call every turn. `changed` is whether anything the screen shows has
    /// moved; `moving` is whether the position is advancing.
    ///
    /// A change inside the coalescing window is **remembered, not dropped** —
    /// `pending` is the difference between coalescing and losing the last
    /// detent of a spin, which would leave the screen one entry behind the
    /// selection until something else happened.
    pub fn poll(&mut self, now: Duration, changed: bool, moving: bool) -> Redraw {
        self.pending |= changed;

        let due = match self.last_full {
            None => true,
            Some(last) => now.saturating_sub(last) >= COALESCE,
        };
        // A `Position` frame here would be followed by the `Full` within the
        // window, which is two frames on the bus for one change — the same
        // waste the position-tick restart below exists to prevent.
        if self.pending && !due {
            return Redraw::Nothing;
        }
        if self.pending && due {
            self.pending = false;
            self.last_full = Some(now);
            // A full draw includes the position, so the slow tick restarts
            // here rather than firing again a moment later.
            self.last_position = Some(now);
            return Redraw::Full;
        }

        if moving {
            let tick_due = match self.last_position {
                None => true,
                Some(last) => now.saturating_sub(last) >= POSITION_EVERY,
            };
            if tick_due {
                self.last_position = Some(now);
                return Redraw::Position;
            }
        }
        Redraw::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> Geometry {
        Geometry { cols: 16, rows: 6, colour: false }
    }

    #[test]
    fn a_name_that_fits_is_left_alone() {
        assert_eq!(fit("short", 16), "short");
        assert_eq!(fit("exactly-16-chars", 16), "exactly-16-chars");
    }

    #[test]
    fn truncation_replaces_a_character_rather_than_adding_one() {
        // One cell too long, so the result must still be 16 cells and not 17.
        let cut = fit("exactly-17-charss", 16);
        assert_eq!(cut.chars().count(), 16);
        assert!(cut.ends_with(TRUNCATED));
    }

    #[test]
    fn a_budget_is_columns_and_not_characters_or_bytes() {
        // All three counts differ, which is the point. Kanji are two columns
        // and ASCII one, so a mixed name fits more characters than a cell
        // count suggests — the error an earlier version of this module made,
        // truncating every Latin name to half the panel.
        let name = "雨音_192k24";
        assert_eq!(name.chars().count(), 9);
        assert_eq!(width(name), 11, "two kanji at two columns each, seven ASCII");
        assert!(name.len() > 11, "pointless unless multibyte");

        // Fits in eleven columns, so it is untouched.
        assert_eq!(fit(name, 11), name);

        // Ten columns: the kanji cost four, leaving five for ASCII plus the
        // marker. Never more than the budget, and never a split character.
        let cut = fit(name, 10);
        assert!(width(&cut) <= 10, "{cut:?} is {} columns", width(&cut));
        assert!(cut.ends_with(TRUNCATED));
        assert!(!cut.contains('\u{fffd}'));

        // A cut that lands where a kanji would straddle the edge drops it
        // rather than half-drawing it.
        let narrow = fit("追憶", 3);
        assert!(width(&narrow) <= 3, "{narrow:?}");
    }

    #[test]
    fn an_ascii_name_is_not_truncated_at_half_the_panel() {
        // The bug this replaces: a 16-column line was treated as sixteen
        // characters of any width, so `test_44k1_16.wav` — sixteen ASCII
        // characters and sixteen columns — was cut for no reason.
        let name = "test_44k1_16.wav";
        assert_eq!(width(name), 16);
        assert_eq!(fit(name, 16), name, "sixteen columns fit sixteen columns");
    }

    #[test]
    fn a_zero_budget_is_empty_rather_than_a_panic() {
        // A panel two cells wide spending two on a mark is absurd, and a
        // display module that panics on absurd input takes the audio with it.
        assert_eq!(fit("anything", 0), "");
        let narrow = Geometry { cols: 1, rows: 1, colour: false };
        assert_eq!(narrow.name_budget(), 0, "one column, and the mark takes it");
        assert_eq!(narrow.listing_rows(), 0);
        assert_eq!(fit("anything", narrow.name_budget()), "");
    }

    #[test]
    fn the_status_line_keeps_the_transport_when_the_rate_will_not_fit() {
        let wide = Geometry { cols: 24, ..geom() };
        let s = status_line(State::Playing, Some(44_100), Some(16), Some(Duration::from_secs(95)), wide);
        assert!(s.contains("PLAY"), "{s}");
        assert!(s.contains("1:35.0"), "{s}");
        assert!(s.contains("44k/16"), "{s}");

        // Narrow: the rate goes, the transport and position stay, and nothing
        // is cut mid-field.
        let narrow = Geometry { cols: 12, ..geom() };
        let s = status_line(State::Playing, Some(44_100), Some(16), Some(Duration::from_secs(95)), narrow);
        assert_eq!(s, "PLAY 1:35.0");
        assert!(!s.contains('~'), "a dropped field should not look like a cut one: {s}");
    }

    #[test]
    fn an_empty_deck_does_not_say_stop() {
        // `transport.rs`: a CDJ has no STOP, and `Stopped` means nothing is
        // loaded rather than that something was halted. Printing STOP would
        // put a word on the panel that the design says does not exist, for
        // the one state it does not describe.
        let wide = Geometry { cols: 24, ..geom() };
        let s = status_line(State::Stopped, Some(44_100), Some(16), Some(Duration::from_secs(95)), wide);
        assert!(!s.contains("STOP"), "{s}");
        assert!(!s.contains("1:35"), "an empty deck has no position: {s}");
        assert!(!s.contains("44k"), "nor a rate in use: {s}");

        // Seeking reads as the buttons are labelled.
        let f = status_line(State::SeekingForward, None, None, Some(Duration::ZERO), wide);
        assert!(f.starts_with("FF"), "{f}");
        let b = status_line(State::SeekingBack, None, None, Some(Duration::ZERO), wide);
        assert!(b.starts_with("REW"), "{b}");
    }

    #[test]
    fn a_position_is_minutes_seconds_and_tenths() {
        assert_eq!(timecode(Duration::ZERO), "0:00.0");
        assert_eq!(timecode(Duration::from_millis(1_500)), "0:01.5");
        assert_eq!(timecode(Duration::from_millis(95_400)), "1:35.4");
        assert_eq!(timecode(Duration::from_secs(3_600)), "60:00.0");
    }

    #[test]
    fn the_listing_leaves_room_for_the_path_and_the_status() {
        let rows: Vec<Row> = (0..10)
            .map(|i| Row::Folder { name: format!("folder-{i}"), selected: i == 0 })
            .collect();
        let screen = compose("録音", &rows[..geom().listing_rows()], "PAUSE".into(), geom());
        assert_eq!(screen.lines.len(), geom().listing_rows());
        assert_eq!(screen.lines.len(), 4, "six rows, less the path and the status");
    }

    #[test]
    fn a_spin_is_coalesced_and_the_last_detent_is_not_lost() {
        // The failure this guards: dropping changes inside the window rather
        // than remembering them leaves the screen one entry behind wherever
        // the spin stopped, until something unrelated redraws it.
        let mut c = Cadence::new();
        assert_eq!(c.poll(Duration::ZERO, true, false), Redraw::Full);

        // Ten detents inside one window: no draws, but not forgotten.
        for i in 1..=10u64 {
            let t = Duration::from_millis(i);
            assert_eq!(c.poll(t, true, false), Redraw::Nothing, "at {i} ms");
        }
        // The window passes with nothing new arriving, and the accumulated
        // change is still drawn.
        assert_eq!(c.poll(COALESCE, false, false), Redraw::Full);
        assert_eq!(c.poll(COALESCE + COALESCE, false, false), Redraw::Nothing);
    }

    #[test]
    fn the_position_ticks_while_playing_and_not_while_stopped() {
        let mut c = Cadence::new();
        assert_eq!(c.poll(Duration::ZERO, true, true), Redraw::Full);

        let t = POSITION_EVERY - Duration::from_millis(1);
        assert_eq!(c.poll(t, false, true), Redraw::Nothing);
        assert_eq!(c.poll(POSITION_EVERY, false, true), Redraw::Position);

        // Stopped: the same second passes and nothing is drawn, which is what
        // keeps an idle deck from waking its panel once a second for ever.
        let mut c = Cadence::new();
        assert_eq!(c.poll(Duration::ZERO, true, false), Redraw::Full);
        assert_eq!(c.poll(POSITION_EVERY * 10, false, false), Redraw::Nothing);
    }

    #[test]
    fn a_full_draw_restarts_the_position_tick() {
        // Otherwise a redraw at 0.9 s is followed by a position draw at 1.0 s,
        // which is two frames on the bus for one second of playing.
        let mut c = Cadence::new();
        assert_eq!(c.poll(Duration::ZERO, true, true), Redraw::Full);
        let late = POSITION_EVERY - Duration::from_millis(100);
        assert_eq!(c.poll(late, true, true), Redraw::Full);
        assert_eq!(c.poll(POSITION_EVERY, false, true), Redraw::Nothing);
        assert_eq!(c.poll(late + POSITION_EVERY, false, true), Redraw::Position);
    }
}

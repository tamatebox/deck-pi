//! The other half: cells into pixels.
//!
//! [`super`] decides what text goes in which cell without a font, a driver or
//! a panel. This draws it, through `embedded-graphics`' `DrawTarget`, so the
//! same code runs against a real controller, against a USB packer, and
//! against the `Vec` of pixels the tests below assert on.
//!
//! **The deck draws; the Pico does not.** `decisions.md`, 2026-09-15: the
//! panel moved to the Pico and "the Pi ships it pixels", deliberately keeping
//! fonts and layout in the one process `CLAUDE.md` describes. So this module
//! is the thing whose output crosses USB, and the `DrawTarget` it is handed
//! will be a packer rather than an SPI driver. It does not know which.
//!
//! # The font is the truth, and it is asked rather than assumed
//!
//! [`super`]'s column arithmetic is a *plan*. Every number below was measured
//! out of the font data instead, twice and independently — once here, once by
//! another session asked for the same figures without being shown these:
//!
//! | | `b12_t_japanese3` | `b16_t_japanese3` |
//! |---|---|---|
//! | ASCII advance | 6 px | 8 px |
//! | full-width advance | 12 px | 16 px |
//! | bounding box | never wider than the advance | same |
//!
//! So a column is exactly half a cell, with no rounding anywhere, and every
//! ASCII glyph is that same width — `W`, `j` and `!` all measure 6 px in
//! `b12`. `column_is_half_a_cell_in_the_font_itself` is that measurement as
//! a test, because it is the assumption the whole layout rests on.
//!
//! Clipping measures the **advance** and not the bounding box: the box is
//! narrower and starts one pixel in, so a selection bar sized to it would
//! leave a gap.
//!
//! # A glyph the face lacks takes the whole line with it
//!
//! Measured, and the reason this module has a [`Report`]:
//! `render("a弘b")` on a face without `弘` draws **nothing at all** and
//! returns `GlyphNotFound` — not "a" and "b" with a hole. u8g2-fonts'
//! `with_ignore_unknown_chars(true)` is worse rather than better: the line
//! renders, the missing character is skipped with *zero* advance, and the
//! name comes out silently shorter than the model planned with the truncation
//! marker in the wrong place.
//!
//! Neither is acceptable on a deck whose library is two-thirds Japanese, so
//! every character is looked up before it is drawn and anything missing is
//! replaced with an ASCII `?`. The substitution is **counted and returned**,
//! not swallowed — `implementation.md`'s *What reads as handled and is not*
//! is the list this module would otherwise have joined.
//!
//! # What the faces are actually missing, counted against the real stick
//!
//! Not guessed and not sampled: `tests/font_covers_library.rs` walked
//! `/media/stick/Music` on 2026-09-16 and asked the faces for every character
//! of every name. **87 of 2192 names contain at least one character neither
//! face has** — 32 distinct characters, and the 12 px and 16 px faces are
//! missing exactly the same set, so this is efont's coverage rather than a
//! size.
//!
//! | | |
//! |---|---|
//! | Latin-1 and Latin Extended, 23 characters | `é`x12, `ã`x10, `ç`x8, `ü`x7, `ñ`x7, `á`, `ä`, `ö`, `õ`, `ú`, `ó`, `ì`, `æ`, `ß`, `É`, `Í`, `ē`, `Ī`, `¥`, `®`, `µ`, soft hyphen, and one Cyrillic `С` inside an otherwise Latin name |
//! | Typographic punctuation, 5 | `″`x8, `…`x7, `‐`x5, `–`x3, zero-width space x2 |
//! | Roman numeral `Ⅱ` | 2 |
//! | **Kanji, 3** | `濱` (`濱瀬元彦`), `繋` (`心を繋ぐ輪`), `鈿` (`螺鈿の箱の秘め事よ`) |
//!
//! The last row is the one worth reading twice. `japanese3` is a *subset* —
//! the grade and JIS-level lists — and this library's names reach outside it,
//! so the assumption that a Japanese face covers a Japanese library is false
//! on this stick by three characters, one of them an artist's name.
//!
//! **None of this blanks a line** — that is what the substitution is for —
//! but a panel spelling `Beyoncé` as `Beyonc?` is showing a name the
//! filesystem does not have. So the first three rows are **folded** rather
//! than substituted, by [`Face::drawn_as`]: `é` draws as `e`, `…` as `...`,
//! `″` as `"`, `Ⅱ` as `II`, and a zero-width space as nothing. That leaves
//! the three kanji and one Cyrillic `С` sitting inside an otherwise Latin
//! name — 5 names of 2192, where a fold would have to invent a reading.
//! Whether those deserve a different face is
//! [#10](https://github.com/tamatebox/deck-pi/issues/10)'s business and not
//! this module's.
//!
//! # Which face, and why not the small one
//!
//! 12 px and 16 px only. The 10 px face is naga10 — "freely usable, but
//! restricted", and unread — where these two are efont, and Public Domain in
//! every source but two. The two are `japanese3`'s thirteen glyphs from
//! baekmuk and Academia Sinica, traced glyph by glyph in
//! [#10](https://github.com/tamatebox/deck-pi/issues/10): both licences
//! permit redistribution and both require their notice to travel with the
//! binary, which is what `NOTICES.md` is. `japanese1` and `japanese2` carry
//! neither, and are not used because their coverage is smaller.
//!
//! There is no separate ASCII face for the status line. `panel-compare` uses
//! one because its 10 px kanji font did not fit a 9 px row; the same trace
//! says the 12 px face's half-width Roman *is* `shnm6x12r`, so a second font
//! would buy nothing and widen the licence surface for no reason.

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use u8g2_fonts::types::{FontColor, VerticalPosition};
use u8g2_fonts::{fonts, Error as FontError, FontRenderer};

use super::{Kind, Line, Screen, TRUNCATED};

/// Drawn in place of a character the face has no glyph for.
///
/// **ASCII, and for the same reason [`super::TRUNCATED`] is**: `□` is itself
/// missing from these faces, so the obvious tofu box would be a substitute
/// that cannot be drawn either.
pub const SUBSTITUTE: char = '?';

/// What a face will put on the panel for a character it does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drawn {
    /// The character itself. The only case where the panel shows the name.
    Itself,
    /// An ASCII stand-in that reads as the character: `é` as `e`, `…` as
    /// `...`, `Ⅱ` as `II`. Not the name, but readable as it.
    Folded(String),
    /// [`SUBSTITUTE`]. Nothing sensible to fall back to.
    Substituted,
}

/// The ASCII stand-in for a character that compatibility decomposition cannot
/// reduce to something the face has.
///
/// **Deliberately short.** Decomposition does the general work — every
/// accented Latin letter, `…`, `Ⅱ`, the fullwidth forms — and a hand-kept
/// table would go stale against the standard exactly as a hand-kept width
/// table would. What is left here is what has no decomposition at all: a
/// ligature whose expansion is a spelling convention, a sign whose ASCII
/// equivalent is a choice, and the invisible characters, which fold to
/// nothing rather than to a space.
fn folded(c: char) -> Option<&'static str> {
    Some(match c {
        'ß' => "ss",
        'æ' => "ae",
        'Æ' => "AE",
        'œ' => "oe",
        'Œ' => "OE",
        'ø' => "o",
        'Ø' => "O",
        'đ' | 'ð' => "d",
        'Đ' => "D",
        'þ' => "th",
        'Þ' => "Th",
        'ł' => "l",
        'Ł' => "L",
        'µ' => "u",
        '¥' => "Y",
        '®' => "(R)",
        '©' => "(C)",
        '×' => "x",
        '÷' => "/",
        '±' => "+/-",
        '°' => "deg",
        // The dashes and quotes a tag editor puts in place of ASCII ones.
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' => "-",
        '\u{2018}' | '\u{2019}' | '\u{201b}' | '\u{2032}' => "'",
        '\u{201c}' | '\u{201d}' | '\u{201f}' | '\u{2033}' => "\"",
        // Invisible, and folding these to a space would insert a word break
        // the filename does not have.
        '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{00ad}' | '\u{feff}' => "",
        _ => return None,
    })
}

/// One pixel of leading between rows, as `tools/panel-compare` uses. Without
/// it the rows touch, which is visible immediately at 12 px where a
/// full-width glyph fills its cell exactly.
pub const LEADING: u32 = 1;

/// The left margin, so a glyph does not start against the glass edge.
const MARGIN_PX: i32 = 1;

static B12: FontRenderer = FontRenderer::new::<fonts::u8g2_font_b12_t_japanese3>();
static B16: FontRenderer = FontRenderer::new::<fonts::u8g2_font_b16_t_japanese3>();

/// Which face, which is also the cell size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    /// `u8g2_font_b12_t_japanese3` — 12 px cells, 6 px columns.
    Px12,
    /// `u8g2_font_b16_t_japanese3` — 16 px cells, 8 px columns.
    Px16,
}

impl Face {
    fn renderer(self) -> &'static FontRenderer {
        match self {
            Face::Px12 => &B12,
            Face::Px16 => &B16,
        }
    }

    /// A full-width cell, square at both sizes.
    pub fn glyph_px(self) -> u32 {
        match self {
            Face::Px12 => 12,
            Face::Px16 => 16,
        }
    }

    /// A column: one half-width glyph. Measured, not divided — see
    /// `column_is_half_a_cell_in_the_font_itself`.
    pub fn col_px(self) -> u32 {
        self.glyph_px() / 2
    }

    /// A row is the cell plus its leading.
    pub fn row_h(self) -> u32 {
        self.glyph_px() + LEADING
    }

    /// The face to use on a panel of this size.
    ///
    /// **The Pico declares pixels, not a font** — the font is the Pi's and the
    /// Pico has no business naming it. So the rule is here: take the larger
    /// face when it still leaves enough listing to browse with, and the
    /// smaller when it does not. Three entries is the floor, which is what
    /// `decisions.md` is arguing about when it says folders-first matters most
    /// on the smallest panel.
    ///
    /// On the candidates that means 16 px for a 320x240 and 12 px for every
    /// 64-pixel-tall panel, where 16 px would leave a single row.
    pub fn for_panel(px_w: u32, px_h: u32) -> Face {
        let big = Painter::new(Face::Px16, Layout { px_w, px_h, colour: false });
        if big.listing_capacity() >= 3 {
            Face::Px16
        } else {
            Face::Px12
        }
    }

    /// How far below `VerticalPosition::Top` a cell actually begins.
    ///
    /// **Not zero, which is the trap.** `Top` is the font's *ascent*, and in
    /// these faces the ideographs stand above it: drawn at `Top` with `y = 0`,
    /// ink appears at `y = -2` in `b12` and `y = -3` in `b16`. The first
    /// version of this module drew each row at its grid position and put 53
    /// pixels off the top edge of a 128x64 panel, where a real driver would
    /// have wrapped them into the last row.
    ///
    /// Measured over **every glyph in both faces** — 3870 and 3875 of them,
    /// not a sample — by `the_ink_of_every_glyph_lands_inside_its_cell`.
    /// With this pad applied the ink occupies exactly `y .. y + glyph_px`,
    /// which is what makes the cell arithmetic elsewhere true.
    pub fn top_pad(self) -> i32 {
        match self {
            Face::Px12 => 2,
            Face::Px16 => 3,
        }
    }

    fn has(self, c: char) -> bool {
        self.renderer()
            .get_rendered_dimensions(c, Point::zero(), VerticalPosition::Top)
            .is_ok()
    }

    /// What this face will actually put on the panel for `c`.
    ///
    /// The fallback is **compatibility decomposition first**: `é` is `e` plus
    /// a combining acute, `…` is three full stops, `Ⅱ` is `II`, and dropping
    /// the marks the face lacks leaves something that reads as the character.
    /// That is one rule rather than a table, and it covers 23 of the 32
    /// characters the real stick turned up. `folded` is the short list of
    /// what decomposition does not reach.
    ///
    /// **A fold can be wider than what it replaces** — `…` is one column and
    /// `...` is three — which the model's budget did not plan for. That is
    /// safe rather than surprising: the renderer measures after folding, so
    /// such a line is clipped and marked rather than run off the panel.
    pub fn drawn_as(self, c: char) -> Drawn {
        if self.has(c) {
            return Drawn::Itself;
        }
        let decomposed: String = c
            .nfkd()
            .filter(|d| !is_combining_mark(*d) && self.has(*d))
            .collect();
        if !decomposed.is_empty() {
            return Drawn::Folded(decomposed);
        }
        match folded(c) {
            Some(f) if f.chars().all(|d| self.has(d)) => Drawn::Folded(f.to_owned()),
            _ => Drawn::Substituted,
        }
    }

    /// The characters of `text` this face can neither draw nor fold, in order
    /// and with repeats — which is what a name-by-name audit wants to print.
    ///
    /// Public because the interesting question is not about one name: it is
    /// whether a whole stick draws, and `tests/font_covers_library.rs` asks
    /// exactly that of `/media/stick/Music`. Empty means every character is
    /// either the filename's own or reads as it.
    pub fn missing(self, text: &str) -> Vec<char> {
        text.chars()
            .filter(|c| self.drawn_as(*c) == Drawn::Substituted)
            .collect()
    }
}

/// The panel, in pixels. The one place a panel choice reaches the drawing.
///
/// Which panel is [#2](https://github.com/tamatebox/deck-pi/issues/2) and is
/// open, so nothing here names a controller: `architecture.md` keeps the model
/// out of the UI code, and this struct is where that promise is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub px_w: u32,
    pub px_h: u32,
    /// Whether a row's kind can be said with a pen instead of a character.
    pub colour: bool,
}

/// The pens. Generic over the colour type so a mono panel and a TFT take the
/// same code path, which is the property `architecture.md` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette<C> {
    pub bg: C,
    pub fg: C,
    /// The path line. On a mono panel this is simply `fg` — there is no dim.
    pub dim: C,
    pub folder: C,
    /// Refused *and* unreadable. Colour distinguishes "will not play" from
    /// "plays"; it does not have a third pen to spend on *why*, which is what
    /// the mono marks carry.
    pub refused: C,
}

impl Palette<BinaryColor> {
    /// Every pen on, except the background.
    pub fn mono() -> Self {
        Palette {
            bg: BinaryColor::Off,
            fg: BinaryColor::On,
            dim: BinaryColor::On,
            folder: BinaryColor::On,
            refused: BinaryColor::On,
        }
    }
}

/// What a draw had to do to the text to make it fit or make it drawable.
///
/// **Returned rather than logged**, and not `()`: a substitution means the
/// panel is showing a name that is not the file's name, and a deck that
/// cannot say so is the failure this module exists to avoid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    /// Characters the face lacks that were folded to an ASCII stand-in which
    /// still reads as them — `é` as `e`, `…` as `...`. See [`Face::drawn_as`].
    pub folded: usize,
    /// Characters replaced by [`SUBSTITUTE`], the face having neither them nor
    /// anything that reads as them. On the stick this is the three kanji
    /// outside `japanese3`.
    pub substituted: usize,
    /// Lines the renderer cut because they measured wider than the panel —
    /// past whatever [`super::fit`] had already done in columns.
    pub clipped: usize,
    /// Characters dropped outright. Only reachable if the face lacks
    /// [`SUBSTITUTE`] itself, which `the_substitute_and_the_marker_exist`
    /// says it does not; a non-zero count here is a bug, not a name.
    pub lost: usize,
}

impl Report {
    pub fn is_clean(self) -> bool {
        self == Report::default()
    }

    fn merge(&mut self, other: Report) {
        self.folded += other.folded;
        self.substituted += other.substituted;
        self.clipped += other.clipped;
        self.lost += other.lost;
    }
}

/// The prefix and suffix a mono panel spends a column on.
///
/// `/` for a folder and `!` for a refused file follow `panel-compare`.
/// Unreadable takes `*`. Not `?`, which is [`SUBSTITUTE`] — a name whose
/// first character the face lacks would draw as a mark it does not have — and
/// not the `x` this first used, which is an ordinary first character for a
/// filename in a way that `!`, `/` and `*` are not.
///
/// On colour, all four are empty and the pen says it instead — which is the
/// capability `decisions.md` credits a colour panel with, and the column it
/// gives back is already in [`super::Geometry::name_budget`].
pub fn marks(kind: Kind, colour: bool) -> (&'static str, &'static str) {
    if colour {
        return ("", "");
    }
    match kind {
        Kind::Folder => ("", "/"),
        Kind::Playable => ("", ""),
        Kind::Refused => ("!", ""),
        Kind::Unreadable => ("*", ""),
    }
}

/// Draws a [`Screen`] onto whatever the panel turns out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Painter {
    pub face: Face,
    pub layout: Layout,
}

impl Painter {
    pub fn new(face: Face, layout: Layout) -> Painter {
        Painter { face, layout }
    }

    /// The grid [`super::compose`] should be given, derived from this face on
    /// this panel rather than typed in beside it.
    ///
    /// That direction matters: a hand-written `Geometry` that disagrees with
    /// the font is a truncation against the wrong budget, which looks like a
    /// shorter filename and nothing else.
    ///
    /// **The margin comes off the width before the columns are counted**, or
    /// the two halves disagree by exactly one pixel: `320 / 8` is 40 columns,
    /// 40 columns of ASCII is 320 px, and [`Painter::prepare`] has 319 px to
    /// put it in — so every line that used its whole budget would come back
    /// with its last character replaced by a truncation marker. Found by
    /// review rather than by a test, because the clipping tests all used the
    /// 128 px panel, where `127 / 6` leaves a pixel spare and hides it.
    /// `a_line_the_model_says_fits_is_never_clipped` is the missing test.
    pub fn geometry(&self) -> super::Geometry {
        super::Geometry {
            cols: (self.room() / self.face.col_px()) as usize,
            rows: 2 + self.listing_capacity(),
            colour: self.layout.colour,
        }
    }

    /// The pixels a line may use: the panel less the margin.
    fn room(&self) -> u32 {
        self.layout.px_w.saturating_sub(MARGIN_PX as u32)
    }

    pub fn face(&self) -> Face {
        self.face
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// The top of the `i`-th listing row, in pixels. Public because the wire
    /// encoder has to say *where* a pen applies, and deriving the row grid a
    /// second time is how two copies of an arithmetic drift apart.
    pub fn row_y(&self, i: usize) -> i32 {
        (self.face.row_h() * (i as u32 + 1)) as i32
    }

    /// Where the status line sits: flush with the bottom edge.
    ///
    /// Bottom-aligned rather than on the grid, so a panel whose height is not
    /// a multiple of the row leaves its slack in the middle where nothing is
    /// drawn, instead of as a band under the status line.
    pub fn status_y(&self) -> i32 {
        self.layout.px_h.saturating_sub(self.face.glyph_px()) as i32
    }

    /// Rows between the path line and the separator. This is the *derivation*
    /// behind `Geometry::listing_rows`, and the two agree by construction —
    /// which is what keeps the bottom listing row from being drawn over the
    /// status line on a panel like 320x240, where dividing the height by the
    /// row height would claim one row too many.
    ///
    /// **The last row is counted without its trailing leading**, and on a
    /// 64 px panel that is a whole extra entry: the naive `px_h / row_h - 2`
    /// leaves 12 of 64 pixels blank above the separator, which is visible on
    /// the first render. `tools/panel-compare`'s `browsable_rows` is the naive
    /// form, so **this disagrees with the sheet issue #2 was judged on** — it
    /// says three browsable rows where the sheet says two, for the 128x64 and
    /// the 256x64 both. `decisions.md` has an argument that turns on the
    /// smallest panel having two, so the disagreement is an input to #2 rather
    /// than a detail of this module.
    pub fn listing_capacity(&self) -> usize {
        let row_h = self.face.row_h();
        let top = row_h;
        let bottom = (self.status_y() as u32).saturating_sub(1);
        ((bottom.saturating_sub(top) + LEADING) / row_h) as usize
    }

    /// The whole screen. [`super::Redraw::Full`].
    ///
    /// This allocates — a `String` per line, a `Vec` in [`Painter::prepare`] —
    /// and that is allowed here and nowhere near the callback: drawing happens
    /// on the control thread, which `architecture.md` says owns every
    /// decision. The crate's allocation invariant is about the audio thread,
    /// which never reaches this module.
    pub fn draw<D, C>(&self, screen: &Screen, pal: &Palette<C>, target: &mut D) -> Result<Report, D::Error>
    where
        D: DrawTarget<Color = C>,
        C: PixelColor,
    {
        target.clear(pal.bg)?;
        let mut report = Report::default();

        report.merge(self.line(&screen.folder, 0, pal.dim, target)?);

        let row_h = self.face.row_h();
        for (i, line) in screen.lines.iter().take(self.listing_capacity()).enumerate() {
            let y = (row_h * (i as u32 + 1)) as i32;
            report.merge(self.listing_line(line, y, pal, target)?);
        }

        // A hairline above the status, so the transport reads as a separate
        // field rather than as another entry in the listing. **After the
        // rows, deliberately**: a selection bar is a whole row tall, so on the
        // bottom entry it reaches the separator's pixel row and would paint
        // over it if these two were swapped.
        let sep = self.status_y() - 1;
        Rectangle::new(Point::new(0, sep), Size::new(self.layout.px_w, 1))
            .into_styled(PrimitiveStyle::with_fill(pal.dim))
            .draw(target)?;

        report.merge(self.draw_status(&screen.status, pal, target)?);
        Ok(report)
    }

    /// The status line alone. [`super::Redraw::Position`] — the small field on
    /// the slow tick that `architecture.md` allows against its own
    /// "update on state change" rule.
    ///
    /// It clears its own row first: without that, `1:09.9` redrawn as `1:10.0`
    /// leaves the tail of the longer glyph behind.
    pub fn draw_status<D, C>(&self, status: &str, pal: &Palette<C>, target: &mut D) -> Result<Report, D::Error>
    where
        D: DrawTarget<Color = C>,
        C: PixelColor,
    {
        let y = self.status_y();
        Rectangle::new(Point::new(0, y), Size::new(self.layout.px_w, self.face.glyph_px()))
            .into_styled(PrimitiveStyle::with_fill(pal.bg))
            .draw(target)?;
        self.line(status, y, pal.fg, target)
    }

    fn listing_line<D, C>(&self, line: &Line, y: i32, pal: &Palette<C>, target: &mut D) -> Result<Report, D::Error>
    where
        D: DrawTarget<Color = C>,
        C: PixelColor,
    {
        let (prefix, suffix) = marks(line.kind, self.layout.colour);
        let mut pen = match line.kind {
            Kind::Folder => pal.folder,
            Kind::Playable => pal.fg,
            Kind::Refused | Kind::Unreadable => pal.refused,
        };

        if line.selected {
            // The bar is the full row including the leading, so consecutive
            // selections would not leave a seam, and it is sized to the
            // advance rather than to the glyph boxes — the boxes start a
            // pixel in and would leave the bar short.
            Rectangle::new(Point::new(0, y), Size::new(self.layout.px_w, self.face.row_h()))
                .into_styled(PrimitiveStyle::with_fill(pen))
                .draw(target)?;
            pen = pal.bg;
        }

        let text = format!("{prefix}{}{suffix}", line.text);
        self.line(&text, y, pen, target)
    }

    /// One line of text whose cell starts at `y`, substituted for what the
    /// face has and clipped to what the panel has.
    ///
    /// `y` is the top of the cell, not the font's idea of it — see
    /// [`Face::top_pad`].
    fn line<D, C>(&self, text: &str, y: i32, pen: C, target: &mut D) -> Result<Report, D::Error>
    where
        D: DrawTarget<Color = C>,
        C: PixelColor,
    {
        let font = self.face.renderer();
        let (drawable, mut report) = self.prepare(text);
        match font.render(
            drawable.as_str(),
            Point::new(MARGIN_PX, y + self.face.top_pad()),
            VerticalPosition::Top,
            FontColor::Transparent(pen),
            target,
        ) {
            Ok(_) => Ok(report),
            Err(FontError::DisplayError(e)) => Err(e),
            // Unreachable: `prepare` looked every character up through the
            // same font, and the colour is `Transparent` so a background is
            // never asked for. Counted rather than ignored, because a line
            // that vanishes with nobody able to say it did is precisely the
            // shape of defect this project keeps finding.
            Err(_) => {
                report.lost += drawable.chars().count();
                Ok(report)
            }
        }
    }

    /// Substitutes what the face cannot draw, then clips to the panel.
    ///
    /// In that order: a substituted `?` is one column where the character it
    /// replaced may have been two, so measuring first would clip against a
    /// width the line no longer has.
    fn prepare(&self, text: &str) -> (String, Report) {
        let font = self.face.renderer();
        let mut report = Report::default();
        let room = self.room();

        let advance = |c: char| -> Option<u32> {
            font.get_rendered_dimensions(c, Point::zero(), VerticalPosition::Top)
                .ok()
                .map(|d| d.advance.x.max(0) as u32)
        };

        // Substitution pass. A fold can be several characters, so this is a
        // `Vec` of what will be drawn rather than of what was asked for.
        let mut subbed: Vec<(char, u32)> = Vec::with_capacity(text.chars().count());
        let push = |c: char, v: &mut Vec<(char, u32)>| match advance(c) {
            Some(px) => {
                v.push((c, px));
                true
            }
            None => false,
        };
        for c in text.chars() {
            match self.face.drawn_as(c) {
                Drawn::Itself => {
                    push(c, &mut subbed);
                }
                Drawn::Folded(f) => {
                    report.folded += 1;
                    for d in f.chars() {
                        push(d, &mut subbed);
                    }
                }
                Drawn::Substituted => {
                    if push(SUBSTITUTE, &mut subbed) {
                        report.substituted += 1;
                    } else {
                        report.lost += 1;
                    }
                }
            }
        }

        // Clipping pass. The model has already cut this to a budget in
        // columns; this is the backstop that makes the font the truth, and it
        // is what catches a character whose East Asian Width and whose glyph
        // disagree.
        let total: u32 = subbed.iter().map(|&(_, px)| px).sum();
        if total <= room {
            return (subbed.into_iter().map(|(c, _)| c).collect(), report);
        }
        report.clipped += 1;
        let marker_px = advance(TRUNCATED).unwrap_or(0);
        let keep = room.saturating_sub(marker_px);
        let mut out = String::new();
        let mut used = 0;
        for (c, px) in subbed {
            if used + px > keep {
                break;
            }
            out.push(c);
            used += px;
        }
        if marker_px > 0 && marker_px <= room {
            out.push(TRUNCATED);
        }
        (out, report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{compose, status_line, Geometry, State};
    use crate::browser::{Row, Verdict};
    use crate::file::{Container, Depth, Reject, TrackInfo};
    use std::time::Duration;

    /// A `DrawTarget` over a `Vec`, with the one property `panel-compare`'s
    /// image target lacks: it **counts** pixels drawn outside the panel
    /// instead of dropping them. A renderer that runs off the edge is exactly
    /// what these tests are for, and a target that silently clips cannot see
    /// it. The real driver would wrap, or write into the next row.
    struct Panel {
        w: u32,
        h: u32,
        px: Vec<BinaryColor>,
        oob: usize,
    }

    impl Panel {
        fn new(w: u32, h: u32) -> Panel {
            Panel { w, h, px: vec![BinaryColor::Off; (w * h) as usize], oob: 0 }
        }

        fn at(&self, x: u32, y: u32) -> BinaryColor {
            self.px[(y * self.w + x) as usize]
        }

        fn lit(&self) -> usize {
            self.px.iter().filter(|c| **c == BinaryColor::On).count()
        }

        /// Lit pixels in the half-open row band `[y0, y1)`.
        fn lit_between(&self, y0: u32, y1: u32) -> usize {
            (y0..y1.min(self.h))
                .flat_map(|y| (0..self.w).map(move |x| (x, y)))
                .filter(|&(x, y)| self.at(x, y) == BinaryColor::On)
                .count()
        }
    }

    impl OriginDimensions for Panel {
        fn size(&self) -> Size {
            Size::new(self.w, self.h)
        }
    }

    impl DrawTarget for Panel {
        type Color = BinaryColor;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(p, c) in pixels {
                if p.x < 0 || p.y < 0 || p.x >= self.w as i32 || p.y >= self.h as i32 {
                    self.oob += 1;
                    continue;
                }
                self.px[(p.y as u32 * self.w + p.x as u32) as usize] = c;
            }
            Ok(())
        }
    }

    /// The 128x64 mono candidate at 12 px, which is the one this would be
    /// built against first.
    fn small() -> Painter {
        Painter::new(Face::Px12, Layout { px_w: 128, px_h: 64, colour: false })
    }

    fn big() -> Painter {
        Painter::new(Face::Px16, Layout { px_w: 320, px_h: 240, colour: true })
    }

    fn folder(name: &str, selected: bool) -> Row {
        Row::Folder { name: name.into(), selected }
    }

    fn track(name: &str, selected: bool) -> Row {
        Row::File {
            name: name.into(),
            verdict: Verdict::Plays(TrackInfo {
                path: name.into(),
                frames: 44_100,
                rate: 44_100,
                channels: 2,
                depth: Depth::Int16,
                container: Container::Aiff,
                seekable: true,
            }),
            selected,
        }
    }

    #[test]
    fn column_is_half_a_cell_in_the_font_itself() {
        // The measurement the whole layout rests on, kept as a test because
        // an earlier version of `display.rs` asserted the opposite from
        // memory and truncated every Latin name to half the panel.
        for face in [Face::Px12, Face::Px16] {
            let f = face.renderer();
            let adv = |s: &str| {
                f.get_rendered_dimensions(s, Point::zero(), VerticalPosition::Top)
                    .expect("in the face")
                    .advance
                    .x as u32
            };
            for c in ["a", "W", "j", "!", "0", " ", "~", "?", "/", "."] {
                assert_eq!(adv(c), face.col_px(), "{c:?} in {face:?} is not one column");
            }
            for c in ["弘", "吉", "村", "追", "憶", "ー", "・", "ァ"] {
                assert_eq!(adv(c), face.glyph_px(), "{c:?} in {face:?} is not one cell");
            }
            assert_eq!(face.col_px() * 2, face.glyph_px());

            // And the model's column count agrees with the font's advance
            // over a real mixed name, which is the arithmetic `fit` does.
            let name = "雨音_192k24";
            assert_eq!(adv(name), crate::display::width(name) as u32 * face.col_px());
        }
    }

    #[test]
    fn the_substitute_and_the_marker_exist() {
        // `Report::lost` is only reachable if these are missing, and the
        // truncation marker is drawn by every line that does not fit.
        for face in [Face::Px12, Face::Px16] {
            for c in [SUBSTITUTE, TRUNCATED] {
                assert!(
                    face.renderer()
                        .get_rendered_dimensions(c, Point::zero(), VerticalPosition::Top)
                        .is_ok(),
                    "{c:?} missing from {face:?}"
                );
            }
        }
    }

    #[test]
    fn the_face_draws_the_library_that_is_on_the_stick() {
        // The test worth having, per the handover: `/media/stick/Music` is
        // 1835 AIFFs of largely Japanese names, and a face that returns
        // `GlyphNotFound` for two thirds of them draws *nothing* — not a
        // partial line, not tofu. Blank rows and a panel nobody can debug.
        let p = small();
        let pal = Palette::mono();
        for name in [
            "吉村弘",
            "追憶のウォーデンクリフ",
            "2024_録音",
            "ライブ音源",
            "夜明けの前.wav",
            "雨音と遠雷_192k24.aiff",
            "圧縮済み.flac",
            "序章.aiff",
            "残響_88k2.wav",
            "屋外採集/2023",
            "ノイズフロア確認.aiff",
            "test_44k1_16.wav",
        ] {
            let mut t = Panel::new(128, 64);
            let r = p.line(name, 0, BinaryColor::On, &mut t).unwrap();
            // Clipping is allowed and expected — `追憶のウォーデンクリフ`
            // measures 132 px against a 128 px panel. A substitution is not:
            // it would mean the panel is showing a name the file does not
            // have, which is the thing worth failing a build over.
            assert_eq!((r.substituted, r.lost), (0, 0), "{name}: {r:?}");
            assert!(t.lit() > 0, "{name} drew no pixels at all");
            assert_eq!(t.oob, 0, "{name} drew {} pixels off the panel", t.oob);
            let _ = &pal;
        }
    }

    #[test]
    fn the_ink_of_every_glyph_lands_inside_its_cell() {
        // The measurement behind `Face::top_pad`, and it is a scan rather than
        // a sample: every codepoint from U+0020 to U+FFFF is asked of the
        // face, and the 3870 (b12) and 3875 (b16) that answer must all draw
        // inside `y .. y + glyph_px` and inside their own advance.
        //
        // Both halves are load-bearing. The vertical one is what the first
        // version of this module got wrong. The horizontal one is what lets
        // `prepare` clip on the advance without leaving a glyph hanging over
        // the edge — asserted here over the whole face rather than trusted.
        for face in [Face::Px12, Face::Px16] {
            let f = face.renderer();
            let cell = face.glyph_px() as i32;
            let mut seen = 0;
            for cp in 0x20u32..=0xFFFF {
                let Some(c) = char::from_u32(cp) else { continue };
                let Ok(d) = f.get_rendered_dimensions(c, Point::new(0, face.top_pad()), VerticalPosition::Top)
                else {
                    continue;
                };
                seen += 1;
                assert!(
                    d.advance.x == face.col_px() as i32 || d.advance.x == cell,
                    "{c:?} in {face:?} advances {} — neither a column nor a cell",
                    d.advance.x
                );
                let Some(bb) = d.bounding_box else { continue };
                assert!(bb.top_left.y >= 0, "{c:?} in {face:?} draws above its cell");
                assert!(
                    bb.top_left.y + bb.size.height as i32 <= cell,
                    "{c:?} in {face:?} draws below its cell"
                );
                assert!(
                    bb.top_left.x + bb.size.width as i32 <= d.advance.x,
                    "{c:?} in {face:?} draws past its advance"
                );
            }
            assert!(seen > 3_800, "{face:?} answered for only {seen} codepoints");
        }
    }

    #[test]
    fn a_glyph_the_face_lacks_does_not_take_the_line_with_it() {
        // Measured: `render` is all-or-nothing. A single `…` picked up from a
        // tag editor would blank the whole row, and `with_ignore_unknown_chars`
        // would instead shorten the name with no marker and no count.
        let p = small();
        let mut t = Panel::new(128, 64);
        let r = p.line("夜明け…の前", 0, BinaryColor::On, &mut t).unwrap();
        assert_eq!((r.folded, r.substituted, r.lost), (1, 0, 0), "{r:?}");
        assert!(t.lit() > 0, "the rest of the line still has to be drawn");
        let (text, _) = p.prepare("夜明け…の前");
        assert_eq!(text, "夜明け...の前");

        // And one with nothing to fall back to. `濱` is on the stick, in
        // `濱瀬元彦`, and is outside `japanese3` — a kanji has no ASCII that
        // reads as it, so this is the case `?` exists for.
        let mut t = Panel::new(128, 64);
        let r = p.line("濱瀬元彦", 0, BinaryColor::On, &mut t).unwrap();
        assert_eq!((r.folded, r.substituted, r.lost), (0, 1, 0), "{r:?}");
        assert_eq!(p.prepare("濱瀬元彦").0, "?瀬元彦");
        assert!(t.lit() > 0);
    }

    #[test]
    fn a_line_the_model_says_fits_is_never_clipped() {
        // **The contract between the two halves**, and the test that was
        // missing: the model's budget is a plan, so the renderer clipping
        // something the model passed means the plan is wrong, not that the
        // backstop worked. It cost a spurious `~` on the last character of
        // every full-width line on the 320 px panel — invisible to every
        // other test here, because they all use the 128 px one.
        for p in [small(), big(), Painter::new(Face::Px12, Layout { px_w: 256, px_h: 64, colour: false })] {
            let g = p.geometry();
            // A name that uses the budget exactly, in both widths, and a
            // mixed one — `fit` is a no-op on all three. The odd column an
            // odd `cols` leaves over goes to an ASCII character, because a
            // kanji cannot have half of one.
            let odd = if g.cols % 2 == 1 { "a" } else { "" };
            for name in [
                "a".repeat(g.cols),
                format!("{}{odd}", "字".repeat(g.cols / 2)),
                format!("{}{}", "音".repeat(g.cols / 4), "x".repeat(g.cols - 2 * (g.cols / 4))),
            ] {
                assert_eq!(crate::display::width(&name), g.cols, "the fixture is not a full line");
                assert_eq!(crate::display::fit(&name, g.cols), name, "the model would not pass this");
                let (drawn, r) = p.prepare(&name);
                assert_eq!(r, Report::default(), "{:?} clipped a full line: {name}", p.face);
                assert_eq!(drawn, name);

                let mut t = Panel::new(p.layout.px_w, p.layout.px_h);
                p.line(&name, 0, BinaryColor::On, &mut t).unwrap();
                assert_eq!(t.oob, 0, "{:?} drew a full line off the panel", p.face);
            }
        }
    }

    #[test]
    fn what_the_face_lacks_is_folded_to_something_that_reads_as_it() {
        // Every one of these is on the real stick. The rule is decomposition
        // first — which is why there is no table entry for any of the
        // accented letters, and why a letter nobody has typed yet will fold
        // too — with the short table behind it for what does not decompose.
        for face in [Face::Px12, Face::Px16] {
            for (c, want) in [
                ('é', "e"),
                ('ã', "a"),
                ('ü', "u"),
                ('ñ', "n"),
                ('ç', "c"),
                ('É', "E"),
                ('ē', "e"),
                ('Ī', "I"),
                ('…', "..."),
                ('Ⅱ', "II"),
                ('ß', "ss"),
                ('æ', "ae"),
                ('µ', "u"),
                ('¥', "Y"),
                ('®', "(R)"),
                ('‐', "-"),
                ('–', "-"),
                ('″', "\""),
                ('\u{200b}', ""),
                ('\u{00ad}', ""),
            ] {
                assert_eq!(
                    face.drawn_as(c),
                    Drawn::Folded(want.to_owned()),
                    "{c:?} in {face:?}"
                );
            }
            // Present, so never folded — the fold must not reach a character
            // the face has. `ー` decomposes to nothing useful and `Ａ` would
            // become `A` if this were applied blindly.
            for c in ['a', '弘', 'ー', '・', 'Ａ', '１'] {
                assert_eq!(face.drawn_as(c), Drawn::Itself, "{c:?} in {face:?}");
            }
            // And the three on the stick that have nowhere to fall back to.
            for c in ['濱', '繋', '鈿'] {
                assert_eq!(face.drawn_as(c), Drawn::Substituted, "{c:?} in {face:?}");
            }
        }
    }

    #[test]
    fn nothing_is_drawn_past_the_edge_of_the_panel() {
        // The model clips in columns; this clips in pixels, and the target
        // counts what escapes rather than dropping it.
        let p = small();
        let mut t = Panel::new(128, 64);
        let long = "第三楽章-長い名前で切り詰めが起きる例と更に続く名前.wav";
        let r = p.line(long, 0, BinaryColor::On, &mut t).unwrap();
        assert_eq!(r.clipped, 1, "{r:?}");
        assert_eq!(t.oob, 0, "{} pixels drawn off the panel", t.oob);

        let (text, _) = p.prepare(long);
        assert!(text.ends_with(TRUNCATED), "{text:?}");
        let px: u32 = text
            .chars()
            .map(|c| {
                p.face
                    .renderer()
                    .get_rendered_dimensions(c, Point::zero(), VerticalPosition::Top)
                    .unwrap()
                    .advance
                    .x as u32
            })
            .sum();
        assert!(px + MARGIN_PX as u32 <= 128, "{px} px plus the margin");
    }

    #[test]
    fn the_grid_comes_from_the_face_and_leaves_the_status_line_alone() {
        // 128x64 at 12 px: 21 columns of 6, a path line, three entries and
        // the transport. 320x240 at 16 px: 39 columns of 8 — 40 would not
        // leave the margin its pixel — and twelve entries.
        assert_eq!(small().geometry(), Geometry { cols: 21, rows: 5, colour: false });
        assert_eq!(big().geometry(), Geometry { cols: 39, rows: 14, colour: true });

        // Whatever the arithmetic, the bottom entry has to end above the
        // status line. This is the assertion the derivation exists for, and
        // it is checked rather than argued.
        for p in [small(), big()] {
            let last = p.face.row_h() * p.listing_capacity() as u32;
            assert!(
                last + p.face.glyph_px() <= p.status_y() as u32,
                "{:?}: the bottom entry reaches {} and the status starts at {}",
                p.face,
                last + p.face.glyph_px(),
                p.status_y()
            );
        }
    }

    #[test]
    fn a_whole_screen_draws_inside_the_panel_and_says_nothing_went_wrong() {
        let p = small();
        let g = p.geometry();
        let rows = [
            folder("2024_録音", false),
            track("追憶のウォーデンクリフ.aiff", true),
            track("test_44k1_16.wav", false),
        ];
        let status = status_line(
            State::Playing,
            Some(44_100),
            Some(16),
            Some(Duration::from_secs(95)),
            g,
        );
        let screen = compose("録音", &rows[..g.listing_rows()], status, g);

        let mut t = Panel::new(128, 64);
        let r = p.draw(&screen, &Palette::mono(), &mut t).unwrap();
        assert!(r.is_clean(), "{r:?}");
        assert_eq!(t.oob, 0);
        // Every band has something in it: the path, both entries, the status.
        assert!(t.lit_between(0, 12) > 0, "the path line is blank");
        assert!(t.lit_between(13, 38) > 0, "the listing is blank");
        assert!(t.lit_between(p.status_y() as u32, 64) > 0, "the status line is blank");
    }

    #[test]
    fn the_selection_is_a_bar_with_the_name_knocked_out_of_it() {
        let p = small();
        let pal = Palette::mono();
        let line = |selected| Line {
            text: "夜明けの前.wav".into(),
            kind: Kind::Playable,
            selected,
        };

        let mut plain = Panel::new(128, 64);
        p.listing_line(&line(false), 13, &pal, &mut plain).unwrap();
        let mut barred = Panel::new(128, 64);
        p.listing_line(&line(true), 13, &pal, &mut barred).unwrap();

        // The bar is the whole row, so the selected draw lights far more of
        // it — and the glyphs come out of the bar rather than over it.
        assert!(barred.lit() > plain.lit() * 2, "{} vs {}", barred.lit(), plain.lit());
        assert_eq!(barred.lit_between(13, 26), (128 * 13) - plain.lit_between(13, 26));
    }

    #[test]
    fn a_mono_panel_spends_a_column_saying_what_colour_would_say_with_a_pen() {
        assert_eq!(marks(Kind::Folder, false), ("", "/"));
        assert_eq!(marks(Kind::Refused, false), ("!", ""));
        assert_eq!(marks(Kind::Unreadable, false), ("*", ""));
        assert_eq!(marks(Kind::Playable, false), ("", ""));
        for kind in [Kind::Folder, Kind::Refused, Kind::Unreadable, Kind::Playable] {
            assert_eq!(marks(kind, true), ("", ""), "colour says it with the pen");
        }

        // The mark costs a column that `name_budget` has already taken off,
        // so a marked line is still one panel wide and not one column over.
        let p = small();
        let g = p.geometry();
        let rows = [Row::File {
            name: "圧縮済みでとても長い名前のファイル.flac".into(),
            verdict: Verdict::Refused(Reject::Depth { found: 0 }),
            selected: false,
        }];
        let screen = compose("録音", &rows, "PAUSE".into(), g);
        let mut t = Panel::new(128, 64);
        let r = p.listing_line(&screen.lines[0], 13, &Palette::mono(), &mut t).unwrap();
        assert_eq!(t.oob, 0);
        assert_eq!(r.clipped, 0, "the model's budget already fits, marks included");
    }

    #[test]
    fn the_position_redraw_touches_only_the_status_row() {
        // `Redraw::Position` exists to put one small field on the bus once a
        // second. If it repainted the listing it would be a full frame with a
        // different name.
        let p = small();
        let pal = Palette::mono();
        let mut t = Panel::new(128, 64);
        p.draw_status("PLAY 1:35.0 44k/16", &pal, &mut t).unwrap();
        assert_eq!(t.lit_between(0, p.status_y() as u32), 0, "drew above the status row");
        assert!(t.lit_between(p.status_y() as u32, 64) > 0);

        // And it clears first, or `1:09.9` leaves its tail behind `1:10.0`.
        let mut t = Panel::new(128, 64);
        p.draw_status("PLAY 10:09.9 192k/24", &pal, &mut t).unwrap();
        let long = t.lit();
        p.draw_status("PLAY 1:10.0", &pal, &mut t).unwrap();
        assert!(t.lit() < long, "the longer line was still on the panel");
    }

}

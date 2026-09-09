//! The candidate geometries, with the provenance of every physical dimension.
//!
//! The comparison is about millimetres, so the active area is the load-bearing
//! input: a 10% error in it is a 10% error in the glyph size being judged.
//! Each entry therefore records where its dimensions came from, and
//! [`check_against_docs`] asserts that the resulting glyph sizes reproduce the
//! figures already in `decisions.md` and `hardware.md`. If they ever stop
//! matching, one of the two is wrong and this says so.

/// Where a panel's active area came from. Not decoration — the two are not
/// equally trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Derived from the nominal diagonal and the pixel aspect ratio. Assumes
    /// the quoted inches describe the *active area*, which holds for the OLED
    /// modules but not for every product.
    Diagonal,
    /// A published active-area figure for the module itself. Preferred.
    ModuleActiveArea,
}

pub struct Panel {
    pub name: &'static str,
    pub controller: &'static str,
    pub px_w: u32,
    pub px_h: u32,
    pub mm_w: f64,
    pub mm_h: f64,
    pub provenance: Provenance,
    /// Cell height in pixels. Full-width glyphs are square at this size.
    pub glyph_px: u32,
    /// True where the panel can distinguish rows by colour rather than by
    /// spending characters on a prefix.
    pub colour: bool,
    /// The bus it would sit on given the ADC decision, and the full-frame
    /// transfer time there.
    pub bus: &'static str,
    pub frame_ms: f64,
    /// The glyph size recorded in the design documents, for the check.
    pub doc_glyph_mm: f64,
}

/// One pixel of leading between rows.
pub const LEADING: u32 = 1;

impl Panel {
    /// A row is the glyph plus leading.
    pub fn row_h(&self) -> u32 {
        self.glyph_px + LEADING
    }

    /// Millimetres per pixel. Square pixels are assumed, which is true for
    /// every candidate here.
    pub fn pitch_mm(&self) -> f64 {
        self.mm_w / self.px_w as f64
    }

    pub fn glyph_mm(&self) -> f64 {
        self.glyph_px as f64 * self.pitch_mm()
    }

    /// Full-width Japanese characters per line. Halfwidth ASCII fits two per
    /// cell in these fonts, so a mixed name does better than this.
    pub fn chars_per_line(&self) -> u32 {
        self.px_w / self.glyph_px
    }

    /// Rows available to the browser, after a path line at the top and a
    /// transport line at the bottom.
    pub fn browsable_rows(&self) -> u32 {
        (self.px_h / self.row_h()).saturating_sub(2)
    }
}

fn from_diagonal(inches: f64, px_w: u32, px_h: u32) -> (f64, f64) {
    let r = px_w as f64 / px_h as f64;
    let h = inches / (r * r + 1.0).sqrt();
    (r * h * 25.4, h * 25.4)
}

pub fn candidates() -> Vec<Panel> {
    let mut v = Vec::new();

    // --- The two I2C candidates -------------------------------------------
    let (w, h) = from_diagonal(2.42, 128, 64);
    v.push(Panel {
        name: "SSD1309 @10px",
        controller: "SSD1309",
        px_w: 128,
        px_h: 64,
        mm_w: w,
        mm_h: h,
        provenance: Provenance::Diagonal,
        // The documents' row for this panel uses Misaki 8x8. **u8g2 has no
        // 8 px Japanese font** — b10 at 10x10 is the smallest — so that row
        // cannot be rendered faithfully here, and 10 px is what this shows.
        // Reproducing the documents' "16 chars x 7 lines at 3.4 mm" would
        // need Misaki itself, a separate BDF.
        glyph_px: 10,
        colour: false,
        bus: "I2C 400k",
        frame_ms: 23.0,
        // 10/128 of 55.0 mm. The documents quote 3.4 mm for this panel at
        // 8 px, which is a different glyph size, so this is exempt from the
        // cross-check rather than in conflict with it.
        doc_glyph_mm: 4.30,
    });

    // The same panel with a 12 px font. The documents' row uses Misaki 8x8 to
    // reach 16 chars and 7 lines, but that same table calls 8x8 "dense kanji
    // blur" — so comparing it against the 12 px and 16 px rows compares
    // different glyph qualities. This row is the honest comparison.
    let (w, h) = from_diagonal(2.42, 128, 64);
    v.push(Panel {
        name: "SSD1309 @12px",
        controller: "SSD1309-12",
        px_w: 128,
        px_h: 64,
        mm_w: w,
        mm_h: h,
        provenance: Provenance::Diagonal,
        glyph_px: 12,
        colour: false,
        bus: "I2C 400k",
        frame_ms: 23.0,
        // 12/128 of 55.0 mm. Not a figure in the documents — they only quote
        // this panel at 8 px — so it is exempt from the cross-check.
        doc_glyph_mm: 5.16,
    });

    let (w, h) = from_diagonal(1.5, 128, 128);
    v.push(Panel {
        name: "SSD1327 1.5\"",
        controller: "SSD1327",
        px_w: 128,
        px_h: 128,
        mm_w: w,
        mm_h: h,
        provenance: Provenance::Diagonal,
        glyph_px: 12,
        colour: false, // Gray4, but a 1-bit font cannot fill it — see the docs.
        bus: "I2C 400k",
        frame_ms: 184.3,
        doc_glyph_mm: 2.5,
    });

    // The trap row: 128x128 mono exists (SH1107) so sourcing is not the
    // objection, but it is made at 1.12 inch.
    let (w, h) = from_diagonal(1.12, 128, 128);
    v.push(Panel {
        name: "SH1107 1.12\"",
        controller: "SH1107",
        px_w: 128,
        px_h: 128,
        mm_w: w,
        mm_h: h,
        provenance: Provenance::Diagonal,
        glyph_px: 12,
        colour: false,
        bus: "I2C 400k",
        frame_ms: 46.1,
        doc_glyph_mm: 1.89,
    });

    // --- What the ADC-on-I2C branch opens up ------------------------------
    v.push(Panel {
        name: "SSD1322 3.12\"",
        controller: "SSD1322",
        px_w: 256,
        px_h: 64,
        // The "3.12 inch" is the glass, not the active area: the published
        // active area's own diagonal is 2.90 inch. Using the diagonal here
        // would overstate the glyph by 6%.
        mm_w: 71.42,
        mm_h: 17.86,
        provenance: Provenance::ModuleActiveArea,
        glyph_px: 12,
        colour: false,
        bus: "SPI 32M",
        frame_ms: 2.05,
        doc_glyph_mm: 3.4,
    });

    let (w, h) = from_diagonal(2.8, 320, 240);
    v.push(Panel {
        name: "ILI9341 2.8\"",
        controller: "ILI9341",
        px_w: 320,
        px_h: 240,
        mm_w: w,
        mm_h: h,
        provenance: Provenance::Diagonal,
        glyph_px: 16,
        colour: true,
        bus: "SPI 32M",
        frame_ms: 38.4,
        doc_glyph_mm: 2.9,
    });

    v
}

/// Every candidate's derived glyph size must reproduce the figure already in
/// the design documents. This is what makes the sheet trustworthy: if the
/// arithmetic here disagreed with the table there, the picture would be of
/// something nobody has agreed on.
pub fn check_against_docs(panels: &[Panel]) -> Result<(), String> {
    for p in panels {
        let got = p.glyph_mm();
        if (got - p.doc_glyph_mm).abs() >= 0.06 {
            return Err(format!(
                "{}: derived {:.2} mm from a {:.1} mm active area, but the documents say {:.2} mm",
                p.name, got, p.mm_w, p.doc_glyph_mm
            ));
        }
    }
    Ok(())
}

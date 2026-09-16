//! A [`Screen`] as a frame on the wire: 1 bpp, plus the pens.
//!
//! [`super::wire`] is the bytes; this is what produces them. The deck draws
//! into a monochrome bitmap with the same [`Painter`] that would drive a real
//! controller, works out which regions are drawn in which pen, and writes one
//! [`Message::Frame`].
//!
//! **Tested against a `Vec<u8>`.** There is no Pico in the loop here and no
//! tty: what is asserted is that the bytes are right, which is the half that
//! can be got wrong quietly. The half that needs hardware — does a real panel
//! light up — needs hardware.
//!
//! # One bit per pixel, and the colour rides beside it
//!
//! The link is the Pico's USB 1.1, so a 16 bpp frame cannot meet the cadence
//! and a 1 bpp one has room to spare (`wire`'s module doc has the arithmetic).
//! That costs nothing visually: the faces are 1-bit bitmaps, and what a colour
//! panel does for this deck is mark *which kind of row this is*, which is one
//! choice per region. So the bitmap is the ink and [`Span`]s say what colour
//! the ink is.
//!
//! Only regions that differ from [`Pen::Fg`] are sent. A listing of ordinary
//! playable files produces a frame with **one** span — the path line — and a
//! folder or a refused row adds one each.

use std::io::Write;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;

use super::paint::{Face, Layout, Painter, Palette};
use super::wire::{Declared, Message, Pen, Reader, Span};
use super::{Geometry, Kind, Lighting, Screen};
use crate::app::panel::Show;

/// A 1 bpp framebuffer: rows padded to whole bytes, most significant bit
/// leftmost, which is what every controller in play wants and what the Pico
/// can blit or expand without rearranging.
pub struct Bitmap {
    w: u32,
    h: u32,
    stride: usize,
    bits: Vec<u8>,
}

impl Bitmap {
    pub fn new(w: u32, h: u32) -> Bitmap {
        let stride = w.div_ceil(8) as usize;
        Bitmap {
            w,
            h,
            stride,
            bits: vec![0; stride * h as usize],
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bits
    }

    pub fn clear_all(&mut self) {
        self.bits.fill(0);
    }

    pub fn get(&self, x: u32, y: u32) -> bool {
        if x >= self.w || y >= self.h {
            return false;
        }
        let byte = self.bits[y as usize * self.stride + (x / 8) as usize];
        byte >> (7 - (x % 8)) & 1 == 1
    }

    fn set(&mut self, x: u32, y: u32, on: bool) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = y as usize * self.stride + (x / 8) as usize;
        let mask = 1u8 << (7 - (x % 8));
        if on {
            self.bits[i] |= mask;
        } else {
            self.bits[i] &= !mask;
        }
    }
}

impl OriginDimensions for Bitmap {
    fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }
}

impl DrawTarget for Bitmap {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 {
                self.set(p.x as u32, p.y as u32, c == BinaryColor::On);
            }
        }
        Ok(())
    }
}

/// The pen a row of this kind is drawn in.
fn pen_of(kind: Kind) -> Pen {
    match kind {
        Kind::Folder => Pen::Folder,
        Kind::Playable => Pen::Fg,
        Kind::Refused | Kind::Unreadable => Pen::Refused,
    }
}

/// Which regions of the frame are not in the default pen.
///
/// Derived from the same [`Painter`] that drew it, so the rectangles cannot
/// drift from the rows they describe.
pub fn spans_of(painter: &Painter, screen: &Screen) -> Vec<Span> {
    let l = painter.layout();
    let row_h = painter.face().row_h();
    let mut spans = Vec::new();

    // The path line, which a colour panel sinks into the background.
    spans.push(Span {
        x0: 0,
        y0: 0,
        x1: l.px_w as u16,
        y1: painter.face().glyph_px() as u16,
        pen: Pen::Dim,
    });

    for (i, line) in screen.lines.iter().take(painter.listing_capacity()).enumerate() {
        let pen = pen_of(line.kind);
        if pen == Pen::Fg {
            continue;
        }
        let y0 = painter.row_y(i) as u16;
        spans.push(Span {
            x0: 0,
            y0,
            x1: l.px_w as u16,
            y1: y0 + row_h as u16,
            pen,
        });
    }
    spans
}

/// Renders a `Screen` onto a panel that is on the far end of a link.
pub struct Packed<W: Write> {
    out: W,
    painter: Painter,
    bitmap: Bitmap,
    declared: Declared,
    seq: u16,
    /// Frames written and not yet acknowledged.
    outstanding: usize,
    /// The last round trip, from write to ack, on the Pi's clock.
    ///
    /// **The figure `architecture.md` has been missing.** It says the cost of
    /// a frame over this link is the number that matters and is unmeasured;
    /// this is it, taken on every frame the deck draws rather than in a
    /// benchmark that would have to imitate one.
    last_rtt: Option<std::time::Duration>,
    sent_at: Option<std::time::Instant>,
    reader: Reader,
}

impl<W: Write> Packed<W> {
    /// Builds a display for what the Pico declared.
    ///
    /// The face is chosen from the panel's pixels by [`Face::for_panel`] — the
    /// Pico declares a size and never a font.
    pub fn new(out: W, declared: Declared) -> Packed<W> {
        let layout = Layout {
            px_w: u32::from(declared.px_w),
            px_h: u32::from(declared.px_h),
            colour: declared.colour,
        };
        let face = Face::for_panel(layout.px_w, layout.px_h);
        Packed {
            out,
            painter: Painter::new(face, layout),
            bitmap: Bitmap::new(layout.px_w, layout.px_h),
            declared,
            seq: 0,
            outstanding: 0,
            last_rtt: None,
            sent_at: None,
            reader: Reader::new(),
        }
    }

    pub fn declared(&self) -> Declared {
        self.declared
    }

    pub fn painter(&self) -> &Painter {
        &self.painter
    }

    pub fn last_rtt(&self) -> Option<std::time::Duration> {
        self.last_rtt
    }

    pub fn outstanding(&self) -> usize {
        self.outstanding
    }

    /// Takes whatever the Pico has said. Acks close out frames and set
    /// [`Self::last_rtt`]; a `Fault` is returned for the caller to act on.
    ///
    /// **Not called from [`Show::show`]'s hot path by accident** — the caller
    /// drives it, because what to do about a `Fault` (go back to the console)
    /// is a decision for the loop and not for a renderer.
    pub fn take(&mut self, from: &mut impl std::io::Read) -> std::io::Result<Vec<Message>> {
        self.reader.fill_from(from)?;
        let mut out = Vec::new();
        while let Some(m) = self.reader.next_message() {
            match m {
                Ok(Message::Ack { .. }) => {
                    self.outstanding = self.outstanding.saturating_sub(1);
                    if let Some(at) = self.sent_at.take() {
                        self.last_rtt = Some(at.elapsed());
                    }
                }
                Ok(other) => out.push(other),
                // A corrupt message is the link's business, and `Reader`
                // counts it; there is nothing for the deck to do about one.
                Err(_) => {}
            }
        }
        Ok(out)
    }

    fn send(&mut self, screen: &Screen) -> std::io::Result<()> {
        self.bitmap.clear_all();
        // `Palette::mono` and not a colour one: the ink is one bit, and which
        // colour it comes out is the pen's business.
        let report = self
            .painter
            .draw(screen, &Palette::mono(), &mut self.bitmap)
            .expect("a Bitmap cannot fail to be drawn on");
        let _ = report;

        self.seq = self.seq.wrapping_add(1);
        let frame = Message::Frame {
            seq: self.seq,
            spans: spans_of(&self.painter, screen),
            bits: self.bitmap.bytes().to_vec(),
        };
        self.out.write_all(&frame.to_bytes())?;
        self.out.flush()?;
        self.outstanding += 1;
        self.sent_at = Some(std::time::Instant::now());
        Ok(())
    }
}

impl<W: Write> Show for Packed<W> {
    fn geometry(&self) -> Geometry {
        self.painter.geometry()
    }

    fn show(&mut self, screen: &Screen) -> std::io::Result<()> {
        self.send(screen)
    }

    /// **Where the Pi's intent becomes the panel's setting.** `Panel` decides
    /// *that* the deck has been idle; what "dim" is belongs to whatever is
    /// plugged in, and `Declared::brightness_levels` is how the Pico said
    /// whether it has any say at all.
    ///
    /// A panel with no levels still blanks — an OLED with no contrast register
    /// can still stop its charge pump, and blanking is the half that matters
    /// for burn-in. It simply does not get a `Brightness` it cannot use.
    fn lighting(&mut self, want: Lighting) -> std::io::Result<()> {
        let levels = self.declared.brightness_levels;
        let mut send = |m: Message| -> std::io::Result<()> {
            self.out.write_all(&m.to_bytes())?;
            self.out.flush()
        };
        match want {
            Lighting::Blank => send(Message::Blank(true))?,
            Lighting::Full | Lighting::Dim => {
                // Unblank first: a brightness set on a panel whose pump is off
                // changes a register nobody can see.
                send(Message::Blank(false))?;
                if levels > 0 {
                    let step = match want {
                        Lighting::Dim => levels / 4,
                        _ => levels - 1,
                    };
                    send(Message::Brightness(step))?;
                }
            }
        }
        Ok(())
    }

    // **No `show_status` override yet, and that is not an oversight.** The
    // Pico declares whether it can take a rectangle, and a partial update
    // needs the Pi to know what is already on the glass — which is state that
    // has to be invalidated on every reconnect, the shape that has bitten this
    // project twice. A whole 1 bpp frame is 1-10 KB and the link carries it;
    // partial updates wait for a measurement that says they are needed.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::wire::{Pen, VERSION};
    use crate::display::{compose, Line};

    fn declared(px_w: u16, px_h: u16, colour: bool) -> Declared {
        Declared {
            px_w,
            px_h,
            colour,
            partial: false,
            brightness_levels: 0,
            max_payload: 16 * 1024,
        }
    }

    fn screen(painter: &Painter) -> Screen {
        let g = painter.geometry();
        let rows: Vec<crate::browser::Row> = vec![
            crate::browser::Row::Folder { name: "2024_録音".into(), selected: false },
            crate::browser::Row::Folder { name: "ライブ音源".into(), selected: true },
        ];
        compose("録音", &rows[..g.listing_rows().min(rows.len())], "PLAY 1:35.0".into(), g)
    }

    fn sent(px_w: u16, px_h: u16) -> (Message, Painter) {
        let mut buf = Vec::new();
        let painter;
        {
            let mut p = Packed::new(&mut buf, declared(px_w, px_h, true));
            painter = *p.painter();
            let s = screen(&painter);
            p.show(&s).expect("show");
        }
        let mut r = Reader::new();
        r.feed(&buf);
        (r.next_message().expect("a frame").expect("not broken"), painter)
    }

    #[test]
    fn a_frame_is_one_bit_per_pixel_and_the_size_says_so() {
        // The whole reason this module exists. 128x64 is 1,024 bytes; the same
        // panel at 16 bpp would be 16,384 and could not be sent at the
        // cadence.
        let (m, _) = sent(128, 64);
        match m {
            Message::Frame { bits, .. } => assert_eq!(bits.len(), 128 * 64 / 8),
            other => panic!("{other:?}"),
        }
        let (m, _) = sent(320, 240);
        match m {
            Message::Frame { bits, .. } => assert_eq!(bits.len(), 320 * 240 / 8, "9600 bytes"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_ink_is_really_in_the_bytes_and_in_the_right_place() {
        // A bitmap that framed correctly and drew nothing would pass every
        // length check above. This asserts the path line has ink in its own
        // rows and that the panel is not simply full.
        let (m, painter) = sent(128, 64);
        let Message::Frame { bits, .. } = m else { panic!() };
        let mut bm = Bitmap::new(128, 64);
        bm.bits.copy_from_slice(&bits);

        let glyph = painter.face().glyph_px();
        let lit_in = |y0: u32, y1: u32| -> usize {
            (y0..y1).flat_map(|y| (0..128u32).map(move |x| (x, y)))
                .filter(|&(x, y)| bm.get(x, y))
                .count()
        };
        assert!(lit_in(0, glyph) > 0, "the path line drew nothing");
        assert!(lit_in(0, 64) < 128 * 64 / 2, "the frame is mostly ink — it is not a screen");
    }

    #[test]
    fn only_what_is_not_the_default_pen_is_sent() {
        // A listing of ordinary files is one span: the path line. Sending a
        // span per row would be correct and wasteful, and the waste is on the
        // link this whole design is shaped by.
        let (m, _) = sent(128, 64);
        let Message::Frame { spans, .. } = m else { panic!() };
        assert!(spans.iter().any(|s| s.pen == Pen::Dim && s.y0 == 0), "{spans:?}");
        assert!(
            spans.iter().all(|s| s.pen != Pen::Fg),
            "the default pen was sent explicitly: {spans:?}"
        );
        // Two folders in the fixture, so two folder spans — but only as many
        // as the panel has room for.
        let folders = spans.iter().filter(|s| s.pen == Pen::Folder).count();
        assert!(folders >= 1, "{spans:?}");
    }

    #[test]
    fn a_span_lands_on_the_row_the_painter_drew() {
        // The rectangles and the glyphs come from one `Painter`, so they
        // cannot disagree — this is what asserts that, rather than the
        // arithmetic being repeated here and agreeing by luck.
        let (m, painter) = sent(320, 240);
        let Message::Frame { spans, .. } = m else { panic!() };
        let row_h = painter.face().row_h() as u16;
        for s in spans.iter().filter(|s| s.pen != Pen::Dim) {
            let i = (s.y0 / row_h).saturating_sub(1) as usize;
            assert_eq!(s.y0, painter.row_y(i) as u16, "{s:?}");
            assert_eq!(s.y1 - s.y0, row_h, "{s:?}");
            assert_eq!(s.x1, painter.layout().px_w as u16);
        }
    }

    #[test]
    fn the_face_follows_the_panel_the_pico_declared() {
        // The Pico declares pixels and never a font. A 64-pixel-tall panel at
        // 16 px would leave one browsable row, so it takes 12; a 240-pixel one
        // takes 16 and gets twelve rows.
        assert_eq!(Face::for_panel(128, 64), Face::Px12);
        assert_eq!(Face::for_panel(256, 64), Face::Px12);
        assert_eq!(Face::for_panel(320, 240), Face::Px16);

        let p = Packed::new(Vec::new(), declared(320, 240, true));
        assert_eq!(p.painter().face(), Face::Px16);
        assert_eq!(p.geometry().rows, 14);
        assert!(p.geometry().colour, "a colour panel still says so");
    }

    #[test]
    fn each_frame_carries_the_next_sequence_number() {
        // Which is what an ack can refer to, and what tells a Pico that it
        // missed one.
        let mut buf = Vec::new();
        {
            let mut p = Packed::new(&mut buf, declared(128, 64, false));
            let s = screen(p.painter());
            for _ in 0..3 {
                p.show(&s).expect("show");
            }
            assert_eq!(p.outstanding(), 3, "nothing has acknowledged them");
        }
        let mut r = Reader::new();
        r.feed(&buf);
        let seqs: Vec<u16> = std::iter::from_fn(|| r.next_message())
            .filter_map(|m| match m.ok() {
                Some(Message::Frame { seq, .. }) => Some(seq),
                _ => None,
            })
            .collect();
        assert_eq!(seqs, vec![1, 2, 3]);
    }

    #[test]
    fn an_ack_closes_the_frame_out_and_times_the_link() {
        // The measurement `architecture.md` is missing, and it arrives as a
        // side effect of drawing rather than as a benchmark that has to
        // imitate drawing.
        let mut buf = Vec::new();
        let mut p = Packed::new(&mut buf, declared(128, 64, false));
        let s = screen(p.painter());
        p.show(&s).expect("show");
        assert_eq!(p.outstanding(), 1);
        assert!(p.last_rtt().is_none());

        let ack = Message::Ack { seq: 1, micros: 42 }.to_bytes();
        let rest = p.take(&mut ack.as_slice()).expect("take");
        assert!(rest.is_empty(), "an ack is not passed on: {rest:?}");
        assert_eq!(p.outstanding(), 0);
        assert!(p.last_rtt().is_some(), "the link was not timed");
    }

    #[test]
    fn a_fault_is_handed_back_rather_than_swallowed() {
        // The panel dying after it declared has to reach the loop — it is how
        // "no panel" becomes a state the deck can re-enter rather than only
        // start in.
        let mut buf = Vec::new();
        let mut p = Packed::new(&mut buf, declared(128, 64, false));
        let fault = Message::Fault { code: 3, reason: "panel gone".into() }.to_bytes();
        let got = p.take(&mut fault.as_slice()).expect("take");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(matches!(&got[0], Message::Fault { code: 3, .. }), "{got:?}");
    }

    #[test]
    fn the_bytes_on_the_wire_are_the_version_this_build_speaks() {
        // Cheap, and it catches the encoder and the constant drifting apart —
        // which would show up at the far end as a Pico refusing every frame.
        let mut buf = Vec::new();
        {
            let mut p = Packed::new(&mut buf, declared(128, 64, false));
            let s = screen(p.painter());
            p.show(&s).expect("show");
        }
        assert_eq!(&buf[..2], b"DP");
        assert_eq!(buf[2], VERSION);
    }

    #[test]
    fn a_selected_row_is_a_bar_of_ink_with_the_name_knocked_out() {
        // The one thing that would look wrong rather than merely wrong-sized:
        // on a colour panel the Pico paints the ink in the row's pen, so a
        // selected row must arrive as mostly-ink-with-holes and not as
        // ordinary text.
        let mut buf = Vec::new();
        let painter;
        {
            let mut p = Packed::new(&mut buf, declared(128, 64, false));
            painter = *p.painter();
            let g = painter.geometry();
            let s = Screen {
                folder: "録音".into(),
                lines: vec![Line {
                    text: "夜明けの前.wav".into(),
                    kind: Kind::Playable,
                    selected: true,
                }],
                status: "PAUSE".into(),
            };
            let _ = g;
            p.show(&s).expect("show");
        }
        let mut r = Reader::new();
        r.feed(&buf);
        let Message::Frame { bits, .. } = r.next_message().unwrap().unwrap() else { panic!() };
        let mut bm = Bitmap::new(128, 64);
        bm.bits.copy_from_slice(&bits);

        let y0 = painter.row_y(0) as u32;
        let row_h = painter.face().row_h();
        let lit = (y0..y0 + row_h)
            .flat_map(|y| (0..128u32).map(move |x| (x, y)))
            .filter(|&(x, y)| bm.get(x, y))
            .count();
        let cells = (row_h * 128) as usize;
        assert!(lit > cells / 2, "the bar is not a bar: {lit} of {cells}");
        assert!(lit < cells, "the name was not knocked out of it");
    }
}

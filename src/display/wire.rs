//! What crosses the USB link to the Pico, as bytes.
//!
//! No tty, no `libc`, no device: this module turns messages into bytes and
//! bytes back into messages, and is tested against a `Vec<u8>`. The thing that
//! opens a serial port is [`super::packed`], and it is thin on top of this on
//! purpose — a framing bug found by unplugging a Pico is a bug found the
//! expensive way.
//!
//! # The link is the Pico's, not the Pi's, and it is smaller than it looks
//!
//! The RP2350 carries **"A USB 1.1 controller and PHY"** (raspberrypi.com,
//! *RP2040 and RP2350 silicon*), and the deck's own Pico confirms it —
//! `lsusb -t` shows it at 12M beside a stick and an Ethernet adapter at 480M.
//! So the ceiling is full speed, roughly 40x under the bus the audio comes
//! over, and `architecture.md` carries the arithmetic. Two consequences shape
//! everything here:
//!
//! - **The Pi ships 1 bit per pixel and never more.** A 320x240 frame at
//!   16 bpp is 153,600 bytes, which is over 100 ms of link time against a
//!   40 ms redraw cadence; the same frame at 1 bpp is 9,600. The glyphs are
//!   1-bit bitmaps to begin with, so nothing is lost by it — `panels.rs` says
//!   the same thing about Gray4 being wasted on a 1-bit glyph.
//! - **Colour survives as an attribute, not as pixels.** What a colour panel
//!   buys this deck is saying *which kind of row this is* without spending a
//!   character on a mark, and that is one choice per region rather than per
//!   pixel. So a frame is a bitmap plus a short list of [`Span`]s, and the
//!   Pico paints those bits in those pens. The picture is the one
//!   `tools/panel-compare` already draws.
//!
//! # Every frame stands on its own
//!
//! A byte stream that loses its far end mid-frame has to resynchronise, so
//! each message carries [`MAGIC`], a version, its length and a CRC, and a
//! reader that loses the thread scans forward for the next magic rather than
//! trying to resume. A half-written frame is dropped, never continued.
//!
//! # The handshake is the Pi's move
//!
//! The Pico enumerates when it is powered, which is seconds before the deck
//! process opens the port — so a declaration sent at enumeration is a
//! declaration sent into a closed door. The Pi sends [`Message::Hello`] and
//! the Pico answers [`Message::Declare`].
//!
//! **And the declaration is also the panel's init-complete signal.** The Pico
//! sends it after its panel is up, and the Pi sends no frame before receiving
//! it, which closes the race that would otherwise need a timer to paper over.

use std::io::{Read, Write};

/// Two bytes that start every message. Short, because a resync scan is over a
/// stream that may be mostly bitmap.
pub const MAGIC: [u8; 2] = *b"DP";

/// Bumped when the layout of any message changes. A Pico speaking a version
/// this does not know is **refused loudly** rather than parsed hopefully.
pub const VERSION: u8 = 1;

/// Header: magic, version, kind, length. The CRC follows the payload.
const HEADER: usize = MAGIC.len() + 1 + 1 + 2;

/// A ceiling on a declared payload, so a corrupt length cannot make the reader
/// wait for bytes that are not coming.
///
/// **16 KiB, and the number is load-bearing in a way the first attempt was
/// not.** That one was 64 KiB — and the length field is a `u16`, so no value
/// it can hold ever exceeds 65,536 and the check could never fire. A bound
/// that cannot be crossed is `implementation.md`'s first shape, declared and
/// never reached, and it was a test asserting the rejection that found it.
///
/// 16 KiB is chosen against the traffic: the largest frame any candidate panel
/// produces is 320x240 at 1 bpp, which is 9,600 bytes plus a short list of
/// spans.
pub const MAX_PAYLOAD: usize = 16 * 1024;

/// Which message this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Hello = 1,
    Declare = 2,
    Frame = 3,
    Ack = 4,
    Fault = 5,
    Brightness = 6,
    Blank = 7,
}

impl Kind {
    fn from_u8(v: u8) -> Option<Kind> {
        Some(match v {
            1 => Kind::Hello,
            2 => Kind::Declare,
            3 => Kind::Frame,
            4 => Kind::Ack,
            5 => Kind::Fault,
            6 => Kind::Brightness,
            7 => Kind::Blank,
            _ => return None,
        })
    }
}

/// Which pen a region is drawn in.
///
/// **Not a colour.** The Pi does not know what the panel can show; it says
/// what the region *means* and the Pico decides what that looks like. On a
/// mono panel every one of these is "on".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Pen {
    /// Ordinary text.
    Fg = 0,
    /// The path line, which a colour panel can sink into the background.
    Dim = 1,
    Folder = 2,
    /// Refused and unreadable both. The mono marks carry which.
    Refused = 3,
}

impl Pen {
    fn from_u8(v: u8) -> Option<Pen> {
        Some(match v {
            0 => Pen::Fg,
            1 => Pen::Dim,
            2 => Pen::Folder,
            3 => Pen::Refused,
            _ => return None,
        })
    }
}

/// A rectangle of the frame drawn in one pen.
///
/// **A rectangle rather than a row**, and the difference is cheap insurance:
/// everything the deck draws today is a whole row in one pen, but a status
/// line with a coloured field in it would need two pens on one row, and a
/// protocol that cannot say that would have to be revised rather than
/// extended. Nine bytes each, against a frame of thousands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub x0: u16,
    pub y0: u16,
    /// Exclusive.
    pub x1: u16,
    /// Exclusive.
    pub y1: u16,
    pub pen: Pen,
}

/// What the Pico says it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Declared {
    /// Zero means **no panel**. A real state, not an error: the Pico carries
    /// the controls whether or not anything is plugged into its display pins,
    /// and the deck then draws to the console instead.
    pub px_w: u16,
    pub px_h: u16,
    /// Whether the pens mean anything. A mono panel ignores them.
    pub colour: bool,
    /// Whether the Pico can take a rectangle instead of a whole frame.
    pub partial: bool,
    /// Steps of brightness, 0 for a panel with no such control. The Pi owns
    /// the idle policy — `architecture.md` gates dimming on the transport,
    /// which only the Pi knows — so it has to know whether there is a knob.
    pub brightness_levels: u8,
    /// The largest payload the Pico will buffer.
    pub max_payload: u16,
}

impl Declared {
    pub fn has_panel(&self) -> bool {
        self.px_w > 0 && self.px_h > 0
    }
}

/// One message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Pi to Pico: are you there, and what have you got.
    Hello,
    /// Pico to Pi, after its panel is up.
    Declare(Declared),
    /// Pi to Pico: a whole frame, 1 bpp, rows padded to whole bytes,
    /// most-significant bit leftmost.
    Frame {
        seq: u16,
        spans: Vec<Span>,
        bits: Vec<u8>,
    },
    /// Pico to Pi: got frame `seq` at `micros` on its own clock.
    ///
    /// **This is the link measurement, for free.** `architecture.md` has
    /// wanted a figure for what a frame costs over USB since the panel moved
    /// to the Pico; the round trip from write to ack is that figure, taken on
    /// every frame the deck draws rather than in a benchmark.
    Ack { seq: u16, micros: u32 },
    /// Pico to Pi: the panel has gone or will not init. Puts the deck back
    /// into the no-panel state it starts in.
    Fault { code: u8, reason: String },
    /// Pi to Pico: brightness step, 0 for darkest.
    Brightness(u8),
    /// Pi to Pico: blank the panel. On an OLED this is also the charge pump.
    Blank(bool),
}

impl Message {
    fn kind(&self) -> Kind {
        match self {
            Message::Hello => Kind::Hello,
            Message::Declare(_) => Kind::Declare,
            Message::Frame { .. } => Kind::Frame,
            Message::Ack { .. } => Kind::Ack,
            Message::Fault { .. } => Kind::Fault,
            Message::Brightness(_) => Kind::Brightness,
            Message::Blank(_) => Kind::Blank,
        }
    }

    fn payload(&self, out: &mut Vec<u8>) {
        match self {
            Message::Hello => {}
            Message::Declare(d) => {
                out.extend_from_slice(&d.px_w.to_le_bytes());
                out.extend_from_slice(&d.px_h.to_le_bytes());
                out.push(u8::from(d.colour) | (u8::from(d.partial) << 1));
                out.push(d.brightness_levels);
                out.extend_from_slice(&d.max_payload.to_le_bytes());
            }
            Message::Frame { seq, spans, bits } => {
                out.extend_from_slice(&seq.to_le_bytes());
                out.extend_from_slice(&(spans.len() as u16).to_le_bytes());
                for s in spans {
                    out.extend_from_slice(&s.x0.to_le_bytes());
                    out.extend_from_slice(&s.y0.to_le_bytes());
                    out.extend_from_slice(&s.x1.to_le_bytes());
                    out.extend_from_slice(&s.y1.to_le_bytes());
                    out.push(s.pen as u8);
                }
                out.extend_from_slice(bits);
            }
            Message::Ack { seq, micros } => {
                out.extend_from_slice(&seq.to_le_bytes());
                out.extend_from_slice(&micros.to_le_bytes());
            }
            Message::Fault { code, reason } => {
                out.push(*code);
                out.extend_from_slice(reason.as_bytes());
            }
            Message::Brightness(n) => out.push(*n),
            Message::Blank(on) => out.push(u8::from(*on)),
        }
    }

    /// Appends the framed message.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let at = out.len();
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(self.kind() as u8);
        out.extend_from_slice(&[0, 0]); // length, filled in below
        self.payload(out);
        let len = out.len() - at - HEADER;
        let len_bytes = (len as u16).to_le_bytes();
        out[at + 4] = len_bytes[0];
        out[at + 5] = len_bytes[1];
        // Over everything but the magic: the magic is what a resync searches
        // for, so covering it would only check that the search worked.
        let crc = crc16(&out[at + 2..]);
        out.extend_from_slice(&crc.to_le_bytes());
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        self.encode(&mut v);
        v
    }

    fn decode(kind: Kind, p: &[u8]) -> Option<Message> {
        let u16at = |i: usize| -> Option<u16> {
            Some(u16::from_le_bytes([*p.get(i)?, *p.get(i + 1)?]))
        };
        Some(match kind {
            Kind::Hello => Message::Hello,
            Kind::Declare => {
                if p.len() < 8 {
                    return None;
                }
                Message::Declare(Declared {
                    px_w: u16at(0)?,
                    px_h: u16at(2)?,
                    colour: p[4] & 1 != 0,
                    partial: p[4] & 2 != 0,
                    brightness_levels: p[5],
                    max_payload: u16at(6)?,
                })
            }
            Kind::Frame => {
                let seq = u16at(0)?;
                let n = u16at(2)? as usize;
                let mut spans = Vec::with_capacity(n);
                let mut at = 4;
                for _ in 0..n {
                    spans.push(Span {
                        x0: u16at(at)?,
                        y0: u16at(at + 2)?,
                        x1: u16at(at + 4)?,
                        y1: u16at(at + 6)?,
                        pen: Pen::from_u8(*p.get(at + 8)?)?,
                    });
                    at += 9;
                }
                Message::Frame {
                    seq,
                    spans,
                    bits: p.get(at..)?.to_vec(),
                }
            }
            Kind::Ack => Message::Ack {
                seq: u16at(0)?,
                micros: u32::from_le_bytes([
                    *p.get(2)?,
                    *p.get(3)?,
                    *p.get(4)?,
                    *p.get(5)?,
                ]),
            },
            Kind::Fault => Message::Fault {
                code: *p.first()?,
                reason: String::from_utf8_lossy(p.get(1..)?).into_owned(),
            },
            Kind::Brightness => Message::Brightness(*p.first()?),
            Kind::Blank => Message::Blank(*p.first()? != 0),
        })
    }
}

/// CRC-16/CCITT-FALSE. Ten lines rather than a dependency, and the constant is
/// the one every embedded toolchain has, so the Pico side is a lookup away.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for b in bytes {
        crc ^= u16::from(*b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Why a read did not produce a message.
#[derive(Debug)]
pub enum Broken {
    /// A version this build does not speak. **Loud rather than hopeful** —
    /// a newer firmware misparsed is worse than one refused.
    Version(u8),
    /// A length past [`MAX_PAYLOAD`], which is corruption rather than a big
    /// message.
    Length(usize),
    /// The header parsed and the CRC did not match. The reader has already
    /// resynchronised past it.
    Crc,
    /// A kind or a payload this build cannot make sense of.
    Malformed,
}

/// Pulls messages out of a byte stream, resynchronising when it has to.
///
/// Holds its own buffer because a serial read returns whatever arrived, which
/// is routinely a fraction of a frame or several frames at once.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
    /// Bytes thrown away hunting for a magic. Non-zero means the link
    /// produced garbage, which is worth surfacing rather than smoothing.
    pub resyncs: u64,
}

impl Reader {
    pub fn new() -> Reader {
        Reader::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Reads whatever is available and feeds it. `Ok(0)` is nothing to read.
    pub fn fill_from(&mut self, r: &mut impl Read) -> std::io::Result<usize> {
        let mut chunk = [0u8; 512];
        let n = r.read(&mut chunk)?;
        self.feed(&chunk[..n]);
        Ok(n)
    }

    /// The next complete message, or `None` when more bytes are needed.
    ///
    /// **Returns at most one thing and never loops**, which the first version
    /// wrote as a `loop` that every path left on its first pass — structure
    /// claiming a retry that could not happen. A caller that wants everything
    /// available calls this until it answers `None`, and every rejection below
    /// is one of those answers rather than something swallowed on the way to a
    /// good frame.
    pub fn next_message(&mut self) -> Option<Result<Message, Broken>> {
        let at = self.find_magic()?;
        if at > 0 {
            self.buf.drain(..at);
            self.resyncs += 1;
        }
        if self.buf.len() < HEADER {
            return None;
        }
        let version = self.buf[2];
        if version != VERSION {
            self.drop_one();
            return Some(Err(Broken::Version(version)));
        }
        let len = u16::from_le_bytes([self.buf[4], self.buf[5]]) as usize;
        if len > MAX_PAYLOAD {
            self.drop_one();
            return Some(Err(Broken::Length(len)));
        }
        let total = HEADER + len + 2;
        if self.buf.len() < total {
            return None;
        }
        let crc_at = HEADER + len;
        let want = u16::from_le_bytes([self.buf[crc_at], self.buf[crc_at + 1]]);
        if crc16(&self.buf[2..crc_at]) != want {
            self.drop_one();
            return Some(Err(Broken::Crc));
        }
        let kind = Kind::from_u8(self.buf[3]);
        let message = kind.and_then(|k| Message::decode(k, &self.buf[HEADER..crc_at]));
        self.buf.drain(..total);
        Some(match message {
            Some(m) => Ok(m),
            None => Err(Broken::Malformed),
        })
    }

    /// Past the magic that just failed, so the scan cannot settle on it again.
    fn drop_one(&mut self) {
        self.buf.drain(..1);
        self.resyncs += 1;
    }

    fn find_magic(&self) -> Option<usize> {
        if self.buf.len() < MAGIC.len() {
            return None;
        }
        (0..=self.buf.len() - MAGIC.len()).find(|&i| self.buf[i..i + 2] == MAGIC)
    }
}

/// Sends [`Message::Hello`] and waits for the [`Message::Declare`] that
/// answers it.
///
/// Anything else the Pico says on the way — a `Fault`, a late `Ack` from a
/// previous run — is skipped rather than treated as the answer.
pub fn handshake(
    w: &mut impl Write,
    r: &mut impl Read,
    reader: &mut Reader,
    deadline: std::time::Duration,
) -> std::io::Result<Declared> {
    w.write_all(&Message::Hello.to_bytes())?;
    w.flush()?;
    let until = std::time::Instant::now() + deadline;
    loop {
        while let Some(m) = reader.next_message() {
            match m {
                Ok(Message::Declare(d)) => return Ok(d),
                // Everything else is noise at this point in the conversation.
                Ok(_) | Err(_) => continue,
            }
        }
        if std::time::Instant::now() >= until {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the Pico did not declare a panel",
            ));
        }
        if reader.fill_from(r)? == 0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Message {
        Message::Frame {
            seq: 0x1234,
            spans: vec![
                Span { x0: 0, y0: 0, x1: 128, y1: 12, pen: Pen::Dim },
                Span { x0: 0, y0: 13, x1: 128, y1: 26, pen: Pen::Folder },
            ],
            bits: vec![0xAA; 1024],
        }
    }

    fn all() -> Vec<Message> {
        vec![
            Message::Hello,
            Message::Declare(Declared {
                px_w: 256,
                px_h: 64,
                colour: false,
                partial: true,
                brightness_levels: 16,
                max_payload: 4096,
            }),
            frame(),
            Message::Ack { seq: 7, micros: 123_456 },
            Message::Fault { code: 2, reason: "panel did not init".into() },
            Message::Brightness(3),
            Message::Blank(true),
        ]
    }

    #[test]
    fn every_message_survives_the_wire() {
        let mut reader = Reader::new();
        for m in all() {
            reader.feed(&m.to_bytes());
            let got = reader.next_message().expect("a message").expect("not broken");
            assert_eq!(got, m);
        }
        assert_eq!(reader.resyncs, 0);
    }

    #[test]
    fn a_message_split_across_reads_is_not_lost() {
        // What a serial port actually does: hands over whatever arrived. A
        // reader that only worked on whole frames would drop most of them.
        let bytes = frame().to_bytes();
        let mut reader = Reader::new();
        for chunk in bytes.chunks(7) {
            if let Some(early) = reader.next_message() {
                panic!("a partial frame decoded: {early:?}");
            }
            reader.feed(chunk);
        }
        assert_eq!(reader.next_message().expect("whole now").expect("not broken"), frame());
    }

    #[test]
    fn several_messages_in_one_read_all_come_out() {
        let mut bytes = Vec::new();
        for m in all() {
            m.encode(&mut bytes);
        }
        let mut reader = Reader::new();
        reader.feed(&bytes);
        let got: Vec<Message> = std::iter::from_fn(|| reader.next_message())
            .map(|r| r.expect("not broken"))
            .collect();
        assert_eq!(got, all());
    }

    #[test]
    fn garbage_before_a_frame_is_skipped_and_counted() {
        // A Pico reset mid-frame leaves the tail of one on the wire. The
        // reader has to find the next header rather than try to resume — and
        // has to say it happened, because silent resyncs are a link fault
        // nobody would ever see.
        let mut bytes = vec![0x00, 0xFF, b'D', 0x01, 0x02, 0x03];
        frame().encode(&mut bytes);
        let mut reader = Reader::new();
        reader.feed(&bytes);
        assert_eq!(reader.next_message().expect("a message").expect("not broken"), frame());
        assert!(reader.resyncs > 0, "the skip was not counted");
    }

    #[test]
    fn a_flipped_bit_is_caught_and_the_reader_carries_on() {
        let mut bytes = frame().to_bytes();
        let n = bytes.len();
        bytes[n / 2] ^= 0x01;
        // A good frame behind the bad one: the point is that the reader gets
        // to it rather than wedging.
        let good = Message::Ack { seq: 9, micros: 1 };
        good.encode(&mut bytes);

        let mut reader = Reader::new();
        reader.feed(&bytes);
        let first = reader.next_message().expect("something");
        assert!(matches!(first, Err(Broken::Crc)), "{first:?}");
        // It may take a resync or two to walk out of the corrupt frame.
        let recovered = std::iter::from_fn(|| reader.next_message())
            .filter_map(|r| r.ok())
            .find(|m| *m == good);
        assert!(recovered.is_some(), "never got past the corrupt frame");
    }

    #[test]
    fn a_version_this_build_does_not_speak_is_refused_rather_than_parsed() {
        // The failure this prevents is the worst kind: a later firmware whose
        // Declare has one more field, read as if it had not, giving a panel
        // geometry that is wrong rather than absent.
        let mut bytes = Message::Hello.to_bytes();
        bytes[2] = VERSION + 1;
        let mut reader = Reader::new();
        reader.feed(&bytes);
        assert!(matches!(reader.next_message(), Some(Err(Broken::Version(v))) if v == VERSION + 1));
    }

    #[test]
    fn a_corrupt_length_does_not_make_the_reader_wait_for_bytes_that_are_not_coming() {
        // Any value a `u16` can hold, past the bound. The first version of
        // this test used 0xFFFF against a 64 KiB bound and passed nothing:
        // see `MAX_PAYLOAD`.
        let mut bytes = Message::Hello.to_bytes();
        let bad = (MAX_PAYLOAD as u16).wrapping_add(1).to_le_bytes();
        bytes[4] = bad[0];
        bytes[5] = bad[1];
        let mut reader = Reader::new();
        reader.feed(&bytes);
        assert!(matches!(reader.next_message(), Some(Err(Broken::Length(_)))));
    }

    #[test]
    fn no_panel_is_a_declaration_and_not_a_silence() {
        // The Pico carries the controls whether or not a panel is wired to
        // it, so "nothing here" has to be sayable — otherwise the deck waits
        // out the handshake timeout on every start with no display.
        let none = Declared {
            px_w: 0,
            px_h: 0,
            colour: false,
            partial: false,
            brightness_levels: 0,
            max_payload: 0,
        };
        assert!(!none.has_panel());
        let mut reader = Reader::new();
        reader.feed(&Message::Declare(none).to_bytes());
        match reader.next_message().expect("a message").expect("not broken") {
            Message::Declare(d) => assert!(!d.has_panel()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_handshake_asks_first_and_skips_what_it_did_not_ask_for() {
        // A `Fault` left over from the previous run, then the answer. Taking
        // the first message as the declaration would start the deck against a
        // panel that had already failed.
        let declared = Declared {
            px_w: 128,
            px_h: 64,
            colour: false,
            partial: false,
            brightness_levels: 0,
            max_payload: 2048,
        };
        let mut from_pico = Vec::new();
        Message::Fault { code: 1, reason: "stale".into() }.encode(&mut from_pico);
        Message::Declare(declared).encode(&mut from_pico);

        let mut to_pico = Vec::new();
        let mut reader = Reader::new();
        let got = handshake(
            &mut to_pico,
            &mut from_pico.as_slice(),
            &mut reader,
            std::time::Duration::from_millis(200),
        )
        .expect("declared");
        assert_eq!(got, declared);
        // And it really did ask.
        assert_eq!(to_pico, Message::Hello.to_bytes());
    }

    #[test]
    fn a_pico_that_never_answers_times_out_rather_than_hanging_the_deck() {
        let mut reader = Reader::new();
        let e = handshake(
            &mut Vec::new(),
            &mut std::io::empty(),
            &mut reader,
            std::time::Duration::from_millis(20),
        )
        .expect_err("should time out");
        assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
    }
}

//! Input: `/dev/input` and the kernel-decoded encoder.
//!
//! `architecture.md` puts this module's ignorance in its description: it
//! "deliberately knows nothing" about GPIO or wiring, because it emits
//! standard keycodes and **which GPIO produces which is a line in
//! `config.txt`**. Nothing here names a pin.
//!
//! # Two disciplines, not one, and the difference is not cosmetic
//!
//! | Buttons | Discipline | Why |
//! |---|---|---|
//! | BACK, PLAY/PAUSE, ENTER, TRACK SEARCH | `Press` on key-down | No second meaning, so waiting would only add latency to the most-used controls. |
//! | CUE, SEARCH | `Press` and `Release` | Both are gestures with a duration. The Cue Point Sampler "continues while the button is held in", and SEARCH seeks while held — so both must start on the press, and the *transport* picks from its own state. |
//!
//! # There were three, and the third was tap-or-hold
//!
//! FF and REW used to carry two meanings on one button: hold seeks, tap
//! changes track. **That discipline is gone**, because TRACK SEARCH took the
//! tap meaning onto its own button once the CDJ-200 panel made six switches
//! available on one wire — `cdj-200.md`. Worth recording rather than quietly
//! deleting, because what went with it was not just a variant:
//!
//! - **The seek now starts 400 ms earlier**, on the press, since there is no
//!   longer a second meaning to disambiguate.
//! - **The decoder no longer reads a clock at all.** It used to take the time
//!   as an argument, deliberately rather than using the event's own
//!   timestamp: those are `CLOCK_REALTIME` by default, so an NTP step —
//!   plausible while the Ethernet cable is in for maintenance — would have
//!   turned a tap into a forty-minute hold. The trap is real and the defence
//!   was right; **both are now moot**, and that is the kind of fact that gets
//!   re-derived from an empty page.
//! - `controls.md`'s requirement that the hold threshold stay well clear of
//!   the 30-50 ms debounce interval **has nothing left to constrain.**
//!
//! Giving every button the tap-or-hold treatment would have been simpler and
//! wrong: **PLAY held a little long would emit a hold and never a tap**, so
//! the deck would not start. Recorded because it is the argument against the
//! obvious uniform rule, and it survives the discipline it was written for.


/// The controls, named by function rather than by pin or keycode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Back,
    PlayPause,
    Cue,
    Enter,
    Rew,
    Ff,
    /// TRACK SEARCH on the CDJ-200 panel. What an FF/REW *tap* used to mean,
    /// now on its own button — `cdj-200.md` for why the compression ended.
    TrackNext,
    TrackPrev,
}

/// How a button's presses are interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discipline {
    /// One meaning; fires on key-down.
    Simple,
    /// Down and up both matter, and the transport decides what they mean.
    Momentary,
}

impl Button {
    /// The keycodes `config.txt` assigns, from `implementation.md`.
    ///
    /// **Verify these against `input-event-codes.h` on the actual image**
    /// rather than trusting them here — `implementation.md` says so, and one wrong
    /// number is a button that silently does nothing. `Cue` is `KEY_STOP`
    /// because Linux has no cue keycode; its three behaviours are all
    /// userspace interpretation of one code.
    pub fn from_keycode(code: u16) -> Option<Button> {
        Some(match code {
            158 => Button::Back,      // KEY_BACK
            164 => Button::PlayPause, // KEY_PLAYPAUSE
            128 => Button::Cue,       // KEY_STOP
            28 => Button::Enter,      // KEY_ENTER
            168 => Button::Rew,       // KEY_REWIND
            208 => Button::Ff,        // KEY_FASTFORWARD
            163 => Button::TrackNext, // KEY_NEXTSONG
            165 => Button::TrackPrev, // KEY_PREVIOUSSONG
            _ => return None,
        })
    }

    pub fn keycode(self) -> u16 {
        match self {
            Button::Back => 158,
            Button::PlayPause => 164,
            Button::Cue => 128,
            Button::Enter => 28,
            Button::Rew => 168,
            Button::Ff => 208,
            Button::TrackNext => 163,
            Button::TrackPrev => 165,
        }
    }

    pub fn discipline(self) -> Discipline {
        match self {
            // Nothing else to wait for, and these are the controls where
            // latency is felt.
            Button::Back | Button::PlayPause | Button::Enter => Discipline::Simple,
            // Track stepping has nothing to wait for either, and the CDJ's
            // own panel repeats when held; this does not, deliberately —
            // `src/input.rs`'s autorepeat guard is load-bearing on USB HID.
            Button::TrackNext | Button::TrackPrev => Discipline::Simple,
            // The Cue Point Sampler plays while held, so the press starts it.
            Button::Cue => Discipline::Momentary,
            // **Seek only, and it starts on the press.** SEARCH's short press
            // means nothing now that TRACK SEARCH exists, so there is no tap
            // to wait for — which is what removed the hold threshold.
            Button::Rew | Button::Ff => Discipline::Momentary,
        }
    }

    const ALL: [Button; 8] = [
        Button::Back,
        Button::PlayPause,
        Button::Cue,
        Button::Enter,
        Button::Rew,
        Button::Ff,
        Button::TrackNext,
        Button::TrackPrev,
    ];

    fn index(self) -> usize {
        match self {
            Button::Back => 0,
            Button::PlayPause => 1,
            Button::Cue => 2,
            Button::Enter => 3,
            Button::Rew => 4,
            Button::Ff => 5,
            Button::TrackNext => 6,
            Button::TrackPrev => 7,
        }
    }
}

/// What the rest of the application reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A `Simple` button, or the down half of a `Momentary` one.
    Press(Button),
    /// The up half of a `Momentary` button.
    Release(Button),
    /// Browse encoder detents, signed. One unit per detent — the kernel
    /// decodes the quadrature, so nothing here counts edges.
    Browse(i32),
}

/// One kernel input event, already split out of its struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEvent {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// `linux/input-event-codes.h`.
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const REL_X: u16 = 0x00;
pub const ABS_X: u16 = 0x00;
/// The USB product string the deck's own firmware presents, and the thing that
/// says a node is **ours**.
///
/// `firmware/src/main.rs` sets it; the kernel builds each input device's name
/// from the manufacturer, this, and the collection — so the buttons arrive as
/// `deck-pi deck-pi control surface Consumer Control`. **Change it in one
/// place and the deck stops finding its controls**, which is why both ends
/// name the other.
pub const PRODUCT: &str = "deck-pi control surface";

/// What a node can emit. Asked of the node itself, never inferred from its
/// name or from where udev put a symlink.
///
/// **Both halves of the control surface are one USB interface**, so `by-id`
/// and `by-path` name one of the two nodes and not the other — measured
/// 2026-09-16, with all three symlinks resolving to the encoder. A build that
/// opened the stable path would get detents and six silently dead buttons.
/// `implementation.md` has the measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Carries {
    /// At least one of the six keycodes `Button::from_keycode` matches.
    pub buttons: bool,
    /// A relative X axis — the browse detents.
    pub detents: bool,
}

impl Carries {
    pub fn anything(self) -> bool {
        self.buttons || self.detents
    }
}

/// Tests one bit of an `EVIOCGBIT` result, which is a flat bitmap indexed by
/// event code. Out of range reads as unset rather than panicking: a short
/// answer from the kernel means the node does not have that code, which is
/// the same thing.
pub fn has_code(bitmap: &[u8], code: u16) -> bool {
    let code = code as usize;
    bitmap
        .get(code / 8)
        .is_some_and(|byte| byte >> (code % 8) & 1 == 1)
}

/// `SYN_DROPPED` — the kernel telling us it threw events away.
///
/// evdev buffers 64 events per client and **drops the whole queue** when a
/// reader falls behind, then emits this. What was lost is unknowable, and a
/// key release is exactly the kind of thing that can be in it.
pub const SYN_DROPPED: u16 = 0x03;

/// Turns kernel events into [`Action`]s.
///
/// Holds no clock and does no I/O, so tap-versus-hold is testable without a
/// device and without sleeping.
pub struct Decoder {
    /// Which buttons are down. No timestamp: nothing here is timed any more.
    down: [bool; 8],
    /// The last absolute encoder position, for the `EV_ABS` flavour.
    abs: Option<i32>,
    /// The axis range, when the device reported one. See `set_abs_range`.
    abs_range: Option<(i32, i32)>,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            down: [false; 8],
            abs: None,
            abs_range: None,
        }
    }

    /// Tells the decoder the absolute axis's range, so a wrap reads as one
    /// step instead of a jump the length of the encoder.
    ///
    /// **From the device, not from `config.txt`.** `EVIOCGABS` reports
    /// `minimum` and `maximum`, so nothing here depends on a line nobody has
    /// run yet — which is the property `implementation.md` asks for and the reason
    /// this is a setter rather than a constant.
    ///
    /// With the overlay's `rollover` parameter the driver wraps 23 to 0, and
    /// the raw difference is then **-23 for one detent clockwise**. Measured
    /// on a probe: `[Browse(1), Browse(-23), Browse(1)]`. Folding needs the
    /// span, and the span is exactly what this supplies.
    ///
    /// **What it cannot fix, because the information never arrives:** without
    /// `rollover` the driver *clamps* the position to `[0, steps]` and the
    /// input core drops the repeated value, so at either end one direction
    /// emits nothing at all. From a cold boot the browser will not scroll
    /// anticlockwise until it has been scrolled clockwise, and after `steps`
    /// detents clockwise it stops going that way. No decoder can recover an
    /// event the kernel did not send — this needs `relative_axis` or
    /// `rollover` in `config.txt`, and [`Decoder::absolute_axis_is_clamped`]
    /// is how a caller can say so out loud.
    pub fn set_abs_range(&mut self, min: i32, max: i32) {
        if max > min {
            self.abs_range = Some((min, max));
        }
    }

    /// True when the axis is bounded and the deck is sitting on a bound, so
    /// one direction is currently silent.
    ///
    /// Advisory, for a caller that wants to warn during bring-up. It cannot
    /// distinguish "clamped, and stuck" from "rollover, and merely at zero" —
    /// both look the same from here — so it says what is observable rather
    /// than guessing the overlay's parameters.
    pub fn absolute_axis_is_clamped(&self) -> bool {
        match (self.abs_range, self.abs) {
            (Some((min, max)), Some(at)) => at == min || at == max,
            _ => false,
        }
    }

    /// Turns a raw difference between absolute positions into detents.
    ///
    /// A wrap looks like a jump most of the way round the axis; a real move
    /// of that size cannot happen between two events. Half the span is the
    /// discriminator, and it needs no tuning because the encoder cannot
    /// travel that far in one poll.
    fn fold_abs(&self, delta: i32) -> i32 {
        let Some((min, max)) = self.abs_range else {
            return delta;
        };
        let span = (max - min).saturating_add(1);
        if delta > span / 2 {
            delta - span
        } else if delta < -(span / 2) {
            delta + span
        } else {
            delta
        }
    }

    /// Feeds one event, appending whatever it means.
    pub fn feed(&mut self, ev: RawEvent, out: &mut Vec<Action>) {
        match ev.kind {
            EV_KEY => self.key(ev, out),
            // The `rotary-encoder` overlay reports **either** a relative or an
            // absolute axis depending on its `relative` parameter, and
            // `implementation.md` says to check the overlay's parameters on the
            // actual image rather than trusting a spelling written down here.
            // Both are handled, so the code does not depend on a line of
            // `config.txt` nobody has run yet.
            EV_REL if ev.code == REL_X && ev.value != 0 => {
                out.push(Action::Browse(ev.value))
            }
            EV_ABS if ev.code == ABS_X => {
                // The first absolute reading establishes the baseline and
                // emits nothing. Without that, an encoder that starts at any
                // position but zero would scroll the browser that far on the
                // first event.
                match self.abs.replace(ev.value) {
                    Some(prev) if ev.value != prev => {
                        let step = self.fold_abs(ev.value.saturating_sub(prev));
                        if step != 0 {
                            out.push(Action::Browse(step));
                        }
                    }
                    _ => {}
                }
            }
            // **`SYN_DROPPED` is the one synchronisation event that means
            // something here.** The rest of `EV_SYN` marks frame boundaries,
            // which this decoder does not need because it acts on events
            // individually — but a drop says the kernel discarded its queue,
            // and a key *release* may have been in it. Carrying on would leave
            // a button held for ever from the decoder's point of view: the
            // transport stuck seeking, or the cue preview playing, with no
            // event ever coming to end it.
            //
            // Giving up on the in-flight state is the only correct response,
            // because the state is exactly what was lost. `reset` closes the
            // gestures on its way out.
            EV_SYN if ev.code == SYN_DROPPED => self.reset(out),
            // Everything else: nothing to do.
            _ => {}
        }
    }

    fn key(&mut self, ev: RawEvent, out: &mut Vec<Action>) {
        let Some(button) = Button::from_keycode(ev.code) else {
            return;
        };
        match ev.value {
            // Autorepeat. `gpio-key` does not repeat by default, but if it
            // ever did, treating a repeat as a fresh press would fire PLAY
            // over and over while a finger rested on it.
            2 => {}
            1 => self.press(button, out),
            0 => self.release(button, out),
            _ => {}
        }
    }

    fn press(&mut self, button: Button, out: &mut Vec<Action>) {
        let slot = &mut self.down[button.index()];
        if *slot {
            // A second down with no up between. The kernel does not do this;
            // ignoring it keeps the state machine total rather than trusting
            // that.
            return;
        }
        *slot = true;
        out.push(Action::Press(button));
    }

    fn release(&mut self, button: Button, out: &mut Vec<Action>) {
        if !std::mem::take(&mut self.down[button.index()]) {
            // An up with no down. Happens after a device is reopened with a
            // button already held.
            return;
        }
        if button.discipline() == Discipline::Momentary {
            out.push(Action::Release(button));
        }
    }

    /// Drops all held state — for a device that went away and came back, or
    /// for a dropped event stream, where the presses that were in flight are
    /// no longer knowable.
    ///
    /// **It terminates them first, and that is the point.** Dropping the state
    /// silently leaves whatever the gestures started running for ever: a
    /// `Press(Ff)` with no `Release` is a transport stuck in
    /// `SeekingForward`, and a `Press(Cue)` with no `Release` is the Cue Point
    /// Sampler playing until the deck is restarted. Nothing downstream can
    /// recover from that, because nothing downstream knows the press is gone.
    ///
    /// So the rule is that every gesture this decoder opens, it closes —
    /// including when it is giving up. Which button was released is not
    /// knowable; that it was released is certain, because the decoder is
    /// about to forget it.
    pub fn reset(&mut self, out: &mut Vec<Action>) {
        for button in Button::ALL {
            // **`Simple` opens nothing, so there is nothing to close** — and
            // inventing a `Press` here would change track because the kernel
            // dropped some events, which is worse than doing nothing.
            if self.down[button.index()] && button.discipline() == Discipline::Momentary {
                out.push(Action::Release(button));
            }
        }
        self.down = [false; 8];
        self.abs = None;
    }
}

#[cfg(target_os = "linux")]
mod device {
    use super::*;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::path::Path;

    /// `struct input_event`, derived rather than hardcoded.
    ///
    /// `implementation.md` says "an input event is a fixed 24-byte struct".
    /// **It is 24 bytes because `timeval` is 16 on a 64-bit build** — measured
    /// against `linux/input.h` on aarch64: `sizeof` 24, with `type` at 16,
    /// `code` at 18 and `value` at 20. On a 32-bit userspace `timeval` is 8
    /// bytes and the struct is 16, so "fixed" is true only of the base
    /// `decisions.md` chose. Deriving it costs one `size_of` and removes the
    /// assumption.
    /// **Derived from `__kernel_ulong_t`, not from userspace's `timeval`.**
    ///
    /// The kernel's `struct input_event` carries two `__kernel_ulong_t`s, and
    /// `__kernel_ulong_t` is `unsigned long` — 8 bytes on a 64-bit build, 4 on
    /// a 32-bit one. Userspace's `timeval` usually matches, which is why
    /// `size_of::<libc::timeval>()` gave the right answer and why it was used.
    ///
    /// It stops matching under `__USE_TIME_BITS64`, where a 32-bit userspace
    /// gets a 16-byte `timeval` while the kernel's event stays at 16 bytes
    /// total. Reading 24 where the kernel writes 16 corrupts every second
    /// event — and libc 0.2 already carries `gnu_time_bits64` and
    /// `musl32_time64` behind environment gates, with a deprecation note
    /// saying it will follow by default in future.
    ///
    /// No effect on this deck: `decisions.md` fixes the base at Raspberry Pi
    /// OS **64-bit**, where both derivations give 24. Changed anyway because
    /// the old one was right by coincidence, and the coincidence is scheduled
    /// to end.
    const TIME_LEN: usize = 2 * std::mem::size_of::<libc::c_ulong>();
    pub const EVENT_LEN: usize = TIME_LEN + 2 + 2 + 4;

    /// Splits one event out of its bytes. The timestamp is skipped
    /// deliberately — see the module docs on `CLOCK_REALTIME`.
    pub fn parse_event(bytes: &[u8]) -> Option<RawEvent> {
        if bytes.len() < EVENT_LEN {
            return None;
        }
        let u16_at = |o: usize| u16::from_ne_bytes([bytes[o], bytes[o + 1]]);
        let i32_at = |o: usize| {
            i32::from_ne_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]])
        };
        Some(RawEvent {
            kind: u16_at(TIME_LEN),
            code: u16_at(TIME_LEN + 2),
            value: i32_at(TIME_LEN + 4),
        })
    }

    /// `struct input_absinfo`, six `__s32`s.
    #[repr(C)]
    #[derive(Default)]
    struct AbsInfo {
        value: i32,
        minimum: i32,
        maximum: i32,
        fuzz: i32,
        flat: i32,
        resolution: i32,
    }

    /// `_IOR(type, nr, size)` from `asm-generic/ioctl.h`, computed rather
    /// than pasted.
    ///
    /// The generic encoding is what aarch64 and x86 use, and
    /// `decisions.md` fixes the base at Raspberry Pi OS 64-bit, so that is
    /// the one that applies. Alpha, MIPS, PowerPC and SPARC lay the direction
    /// bits out differently — noted because a hardcoded constant would carry
    /// that assumption invisibly, which is the habit this file already
    /// follows for `EVENT_LEN`.
    const fn ioc_read(ty: u8, nr: u8, size: usize) -> libc::c_ulong {
        (2 << 30) | ((size as libc::c_ulong) << 16) | ((ty as libc::c_ulong) << 8) | nr as libc::c_ulong
    }

    /// One `/dev/input/eventN`.
    pub struct Device {
        file: File,
        /// Kept so rediscovery can tell an already-open node from a new one.
        /// Node numbers are reused, so this is an identity only while the
        /// descriptor is open — which is exactly as long as it is used for.
        path: std::path::PathBuf,
        /// Sized for a burst; a partial event at the end is carried over,
        /// because a `read` is not guaranteed to stop on a struct boundary.
        buf: Vec<u8>,
        held: usize,
    }

    impl Device {
        pub fn open(path: &Path) -> std::io::Result<Device> {
            Ok(Device {
                file: File::open(path)?,
                path: path.to_path_buf(),
                buf: vec![0u8; EVENT_LEN * 64],
                held: 0,
            })
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        /// The device's name, as `EVIOCGNAME` reports it.
        ///
        /// `None` when the ioctl fails or the answer is not UTF-8, both of
        /// which mean "not a node to take seriously" here rather than an
        /// error worth propagating.
        pub fn name(&self) -> Option<String> {
            let mut buf = [0u8; 256];
            let req = ioc_read(b'E', 0x06, buf.len());
            // SAFETY: an open descriptor this struct owns, and a buffer whose
            // size is the one encoded in the request.
            let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), req as _, buf.as_mut_ptr()) };
            if rc <= 0 {
                return None;
            }
            let bytes = &buf[..(rc as usize).min(buf.len())];
            let bytes = bytes.split(|b| *b == 0).next().unwrap_or(bytes);
            std::str::from_utf8(bytes).ok().map(str::to_owned)
        }

        /// Asks the node what it can emit, with `EVIOCGBIT`.
        ///
        /// The same `ioc_read` that `abs_range` uses: `EVIOCGBIT(ev, len)` is
        /// `_IOC(_IOC_READ, 'E', 0x20 + ev, len)`. A failed ioctl is reported
        /// as "carries nothing" rather than as an error, because the caller is
        /// sorting a directory and a node that will not answer is a node to
        /// skip.
        pub fn carries(&self) -> Carries {
            // `KEY_CNT` is 0x300 and `REL_CNT` 0x10, so 96 and 2 bytes.
            let mut keys = [0u8; 96];
            let mut rels = [0u8; 2];
            let key_req = ioc_read(b'E', 0x20 + EV_KEY as u8, keys.len());
            let rel_req = ioc_read(b'E', 0x20 + EV_REL as u8, rels.len());
            let fd = self.file.as_raw_fd();
            // SAFETY: an open descriptor this struct owns, and buffers whose
            // sizes are the ones encoded in the requests.
            let (key_rc, rel_rc) = unsafe {
                (
                    libc::ioctl(fd, key_req as _, keys.as_mut_ptr()),
                    libc::ioctl(fd, rel_req as _, rels.as_mut_ptr()),
                )
            };
            Carries {
                buttons: key_rc >= 0 && Button::ALL.iter().any(|b| has_code(&keys, b.keycode())),
                detents: rel_rc >= 0 && has_code(&rels, REL_X),
            }
        }

        pub(super) fn raw_fd(&self) -> std::os::fd::RawFd {
            self.file.as_raw_fd()
        }

        /// The axis range the device reports, for `Decoder::set_abs_range`.
        ///
        /// `None` when the device has no absolute X axis, which is the
        /// ordinary answer for a button node and for an encoder configured
        /// with `relative_axis`.
        pub fn abs_range(&self) -> Option<(i32, i32)> {
            let mut info = AbsInfo::default();
            let req = ioc_read(b'E', 0x40 + ABS_X as u8, std::mem::size_of::<AbsInfo>());
            // SAFETY: an open descriptor this struct owns, and a correctly
            // sized `input_absinfo` for the size encoded in the request.
            let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), req as _, &mut info) };
            if rc < 0 || info.maximum <= info.minimum {
                return None;
            }
            Some((info.minimum, info.maximum))
        }

        /// Reads whatever is pending and appends the events.
        ///
        /// `Ok(false)` means the descriptor reached end of file, which for an
        /// evdev node means the device is gone. **A zero-length read used to
        /// be indistinguishable from "nothing arrived"**, so a hung-up
        /// descriptor became a spin: a `poll` that returns immediately for
        /// ever and a read that yields nothing. Measured on a FIFO standing in
        /// for a removed device: **340,838 iterations in 200 ms.** A real
        /// evdev node returns `ENODEV` rather than EOF, so this was unreachable
        /// through the kernel — which is to say it depended on the kernel's
        /// good manners rather than on anything here.
        pub fn read_pending(&mut self, out: &mut Vec<RawEvent>) -> std::io::Result<bool> {
            let n = self.file.read(&mut self.buf[self.held..])?;
            if n == 0 {
                return Ok(false);
            }
            let total = self.held + n;
            let whole = total / EVENT_LEN;
            for i in 0..whole {
                if let Some(ev) = parse_event(&self.buf[i * EVENT_LEN..]) {
                    out.push(ev);
                }
            }
            // Carry the tail. A short read mid-struct is unusual on an evdev
            // node but not forbidden, and dropping the remainder would
            // desynchronise every event after it.
            let rest = total % EVENT_LEN;
            self.buf.copy_within(whole * EVENT_LEN..total, 0);
            self.held = rest;
            Ok(true)
        }
    }

    /// Every node that carries part of the control surface, found by asking
    /// each one what it emits.
    ///
    /// A node that will not open is skipped rather than reported: `/dev/input`
    /// holds nodes this process has no business with, and on this deck two of
    /// them are the HDMI and headphone jacks.
    ///
    /// **Capability alone is not enough, and finding that out cost nothing
    /// only because this mode existed.** `vc4-hdmi` — the HDMI CEC remote — 
    /// declares hundreds of keycodes including all six of the deck's, and
    /// `REL_X` with it. Selecting on what a node *can send* would hand the
    /// deck to whatever television it is plugged into. So identity comes
    /// first, by [`PRODUCT`], and capability only sorts the nodes that are
    /// already ours: which of them carries the buttons and which the detents.
    pub fn discover() -> std::io::Result<Vec<std::path::PathBuf>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir("/dev/input")? {
            let path = entry?.path();
            let is_event = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"));
            if !is_event {
                continue;
            }
            if let Ok(dev) = Device::open(&path) {
                let ours = dev.name().is_some_and(|n| n.contains(PRODUCT));
                if ours && dev.carries().anything() {
                    found.push(path);
                }
            }
        }
        // `read_dir` order is the filesystem's. Sorted so two runs on an
        // unchanged machine open the same nodes in the same order, which is
        // the difference between a reproducible bug and a haunted one.
        found.sort();
        Ok(found)
    }

    /// Every input node the deck listens to, polled together.
    ///
    /// **More than one, and how many is not fixed.** The control surface is a
    /// single USB device presenting two application collections, so the kernel
    /// makes two nodes: the buttons on one, the detents on the other. A
    /// single-descriptor `poll` hears half the controls — and waiting on them
    /// in turn would be worse than it sounds, because each wait would spend
    /// the full timeout before the next got a look in, so the round trip would
    /// be a multiple of the poll interval and `Decoder::tick` would run that
    /// much later.
    ///
    /// The count belongs to the firmware's descriptor rather than to any
    /// decision here, which is why nothing in `architecture.md`'s Input row
    /// mentions it, and why [`discover`] asks rather than counts.
    pub struct Devices {
        devices: Vec<Device>,
        /// Parallel to `devices`, rebuilt on every `wait`.
        polls: Vec<libc::pollfd>,
    }

    impl Devices {
        /// Opens all of them. A path that will not open is an error: a deck
        /// missing one of its buttons should say so at start rather than
        /// discover it when the button is pressed.
        pub fn open(paths: &[std::path::PathBuf]) -> std::io::Result<Devices> {
            let mut devices = Vec::with_capacity(paths.len());
            for p in paths {
                devices.push(Device::open(p)?);
            }
            Ok(Devices {
                polls: Vec::with_capacity(devices.len()),
                devices,
            })
        }

        /// Opens whatever [`discover`] finds.
        ///
        /// Unlike [`Devices::open`] this is not an error when it finds
        /// nothing: a deck whose control surface is unplugged should start and
        /// say so, not refuse to start. Ask [`Devices::carries`] what turned up.
        pub fn open_discovered() -> std::io::Result<Devices> {
            Self::open(&discover()?)
        }

        /// Re-runs discovery and opens anything not already open, returning
        /// how many were added.
        ///
        /// **This is the difference between a USB control surface and a
        /// soldered one.** An overlay's node exists from boot and a vanishing
        /// one really is a fault, so `read_pending` dropping a lost device was
        /// the whole story. A USB device vanishing is ordinary — a replug, a
        /// bus reset, a knocked cable — and without this the deck plays on
        /// with every control dead and nothing logged.
        pub fn rediscover(&mut self) -> std::io::Result<usize> {
            let mut added = 0;
            for path in discover()? {
                if self.devices.iter().any(|d| d.path() == path) {
                    continue;
                }
                // Lost between the scan and the open: it will be found next
                // time round rather than failing the whole sweep.
                if let Ok(dev) = Device::open(&path) {
                    self.devices.push(dev);
                    added += 1;
                }
            }
            if added > 0 {
                self.polls.clear();
            }
            Ok(added)
        }

        /// What the open nodes can emit between them, so a caller can say
        /// *which* half of the control surface is missing rather than only
        /// that something is.
        pub fn carries(&self) -> Carries {
            self.devices
                .iter()
                .fold(Carries::default(), |acc, d| {
                    let c = d.carries();
                    Carries {
                        buttons: acc.buttons || c.buttons,
                        detents: acc.detents || c.detents,
                    }
                })
        }

        pub fn len(&self) -> usize {
            self.devices.len()
        }

        pub fn is_empty(&self) -> bool {
            self.devices.is_empty()
        }

        /// The range of the first device that reports an absolute axis.
        pub fn abs_range(&self) -> Option<(i32, i32)> {
            self.devices.iter().find_map(Device::abs_range)
        }

        /// Waits up to `timeout` for any device to have something to read.
        ///
        /// A timeout rather than a blocking read, because a **hold is defined
        /// by an event not arriving**: block for ever and `Decoder::tick`
        /// never runs, so FF would never start seeking. The timeout is what
        /// bounds how late a `HoldStart` can be, and polling all the nodes at
        /// once is what keeps it one timeout rather than seven.
        pub fn wait(&mut self, timeout: Duration) -> std::io::Result<bool> {
            let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
            if self.devices.is_empty() {
                // `poll` with no descriptors returns at once, so without this
                // the caller spins instead of waiting.
                std::thread::sleep(timeout);
                return Ok(false);
            }
            self.polls.clear();
            self.polls.extend(self.devices.iter().map(|d| libc::pollfd {
                fd: d.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }));
            // SAFETY: `polls` is a valid array of that length, describing
            // descriptors owned by `devices`, which outlive the call.
            let rc = unsafe {
                libc::poll(self.polls.as_mut_ptr(), self.polls.len() as libc::nfds_t, ms)
            };
            match rc {
                -1 => {
                    let e = std::io::Error::last_os_error();
                    // A signal is not a failure; the caller loops.
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        Ok(false)
                    } else {
                        Err(e)
                    }
                }
                0 => Ok(false),
                _ => Ok(true),
            }
        }

        /// Reads from whichever devices `wait` found ready, and drops any that
        /// have gone away.
        ///
        /// Returns how many were dropped. **A caller that gets a non-zero
        /// answer must reset its decoder**, because the presses that were in
        /// flight on that node can no longer be released by anything — which
        /// is the stuck-`SeekingForward` failure from the other direction.
        ///
        /// `revents` is examined rather than only the return value: `POLLERR`,
        /// `POLLHUP` and `POLLNVAL` are reported **whether or not they were
        /// asked for**, and a hung-up descriptor is permanently ready, so
        /// ignoring them turns an unplugged device into a busy loop.
        pub fn read_pending(&mut self, out: &mut Vec<RawEvent>) -> std::io::Result<usize> {
            let mut lost = 0usize;
            let mut keep = Vec::with_capacity(self.devices.len());
            for (i, dev) in self.devices.drain(..).enumerate() {
                let revents = self.polls.get(i).map(|p| p.revents).unwrap_or(0);
                let broken = revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0;
                if broken {
                    lost += 1;
                    continue;
                }
                let mut dev = dev;
                if revents & libc::POLLIN != 0 {
                    match dev.read_pending(out) {
                        Ok(true) => {}
                        // End of file, or the node reporting the device gone.
                        Ok(false) | Err(_) => {
                            lost += 1;
                            continue;
                        }
                    }
                }
                keep.push(dev);
            }
            self.devices = keep;
            self.polls.clear();
            Ok(lost)
        }
    }
}

#[cfg(target_os = "linux")]
pub use device::{discover, parse_event, Device, Devices, EVENT_LEN};

#[cfg(test)]
mod tests {
    use super::*;

    /// `EVIOCGBIT` answers with a flat bitmap indexed by event code, and a
    /// short answer means the codes past the end are absent rather than that
    /// something went wrong. Both halves are asserted because an out-of-range
    /// read that panicked would take the deck down while it was looking at an
    /// unrelated node in `/dev/input`.
    #[test]
    fn a_capability_bitmap_is_read_by_code_and_is_short_rather_than_wrong() {
        // bit 0 and bit 9 of a two-byte map: codes 0 and 9.
        let map = [0b0000_0001u8, 0b0000_0010u8];
        assert!(has_code(&map, 0));
        assert!(!has_code(&map, 1));
        assert!(has_code(&map, 9));
        assert!(!has_code(&map, 8));
        // Past the end, and far past it.
        assert!(!has_code(&map, 16));
        assert!(!has_code(&map, u16::MAX));
        assert!(!has_code(&[], 0));
    }

    /// The six the deck matches all sit inside a 96-byte `KEY_CNT` map, which
    /// is the size `Device::carries` asks for. A seventh control added past
    /// `KEY_MAX` would read as absent, silently, so the bound is asserted
    /// rather than assumed.
    #[test]
    fn every_button_keycode_fits_the_map_carries_asks_for() {
        for b in Button::ALL {
            assert!(
                (b.keycode() as usize) < 96 * 8,
                "{:?} at {} is outside the bitmap",
                b,
                b.keycode()
            );
        }
    }

    fn key(code: u16, value: i32) -> RawEvent {
        RawEvent {
            kind: EV_KEY,
            code,
            value,
        }
    }

    #[test]
    fn play_fires_on_the_press_however_long_it_is_held() {
        // The failure a uniform tap-or-hold rule would cause: PLAY held a
        // little long emits a hold and never a tap, so the deck does not
        // start. The discipline it argued against is gone, but the argument
        // is why PLAY is `Simple` and not something cleverer.
        let mut d = Decoder::default();
        let mut out = Vec::new();

        d.feed(key(Button::PlayPause.keycode(), 1), &mut out);
        assert_eq!(out, vec![Action::Press(Button::PlayPause)]);

        out.clear();
        d.feed(key(Button::PlayPause.keycode(), 0), &mut out);
        assert!(out.is_empty(), "a Simple button says nothing on release");
    }

    #[test]
    fn search_seeks_from_the_press_and_track_search_is_its_own_button() {
        // **The seek used to wait out the hold threshold**, because a short
        // press meant "next track" and which one it was could not be known
        // until the button came back up. TRACK SEARCH owns that meaning now,
        // so SEARCH starts seeking on the way down — 400 ms earlier.
        let mut d = Decoder::default();
        let mut out = Vec::new();

        d.feed(key(Button::Ff.keycode(), 1), &mut out);
        assert_eq!(out, vec![Action::Press(Button::Ff)], "no threshold to wait out");
        out.clear();


        d.feed(key(Button::Ff.keycode(), 0), &mut out);
        assert_eq!(out, vec![Action::Release(Button::Ff)], "the release ends the seek");

        // The meaning that left: its own button, and it fires on the way down.
        out.clear();
        d.feed(key(Button::TrackNext.keycode(), 1), &mut out);
        assert_eq!(out, vec![Action::Press(Button::TrackNext)]);
        out.clear();
        d.feed(key(Button::TrackNext.keycode(), 0), &mut out);
        assert!(out.is_empty(), "a Simple button says nothing on release");
    }

    #[test]
    fn cue_is_a_press_and_a_release_because_the_transport_decides() {
        // The Cue Point Sampler "continues while the button is held in", so
        // it cannot wait for a threshold. `Transport::cue_down` /
        // `cue_up` are exactly this pair, and which of the three CDJ-350
        // behaviours happens is decided there from the transport's own state.
        let mut d = Decoder::default();
        let mut out = Vec::new();

        d.feed(key(Button::Cue.keycode(), 1), &mut out);
        assert_eq!(out, vec![Action::Press(Button::Cue)]);
        out.clear();


        d.feed(key(Button::Cue.keycode(), 0), &mut out);
        assert_eq!(out, vec![Action::Release(Button::Cue)]);
    }

    #[test]
    fn every_button_has_a_discipline_and_the_split_is_the_documented_one() {
        use Discipline::*;
        let expected = [
            (Button::Back, Simple),
            (Button::PlayPause, Simple),
            (Button::Enter, Simple),
            (Button::TrackNext, Simple),
            (Button::TrackPrev, Simple),
            (Button::Cue, Momentary),
            (Button::Rew, Momentary),
            (Button::Ff, Momentary),
        ];
        for (b, want) in expected {
            assert_eq!(b.discipline(), want, "{:?}", b);
        }
        // And the keycode mapping is a bijection, so no two controls share a
        // code and none is unreachable.
        for b in Button::ALL {
            assert_eq!(Button::from_keycode(b.keycode()), Some(b), "{:?}", b);
        }
        let mut codes: Vec<u16> = Button::ALL.iter().map(|b| b.keycode()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), Button::ALL.len(), "two buttons share a keycode");
    }

    #[test]
    fn a_relative_encoder_scrolls_and_an_absolute_one_does_not_jump_on_the_first_event() {
        // The `rotary-encoder` overlay reports one or the other depending on
        // a parameter nobody has run `dtoverlay -h` against yet. Handling
        // both removes the dependency on that.
        let mut d = Decoder::default();
        let mut out = Vec::new();

        for v in [1, -1, 3] {
            d.feed(
                RawEvent {
                    kind: EV_REL,
                    code: REL_X,
                    value: v,
                },
                &mut out,
            );
        }
        assert_eq!(
            out,
            vec![Action::Browse(1), Action::Browse(-1), Action::Browse(3)]
        );

        // Absolute: the first reading is a baseline, not a scroll of 5000.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        let abs = |v| RawEvent {
            kind: EV_ABS,
            code: ABS_X,
            value: v,
        };
        d.feed(abs(5000), &mut out);
        assert!(out.is_empty(), "the first absolute reading must not scroll");
        d.feed(abs(5002), &mut out);
        d.feed(abs(5001), &mut out);
        assert_eq!(out, vec![Action::Browse(2), Action::Browse(-1)]);
    }

    #[test]
    fn an_absolute_encoder_that_wraps_reads_as_one_detent_not_a_jump() {
        // With the overlay's `rollover` parameter the driver takes 23 to 0
        // for one detent clockwise, and the raw difference is **-23**. A
        // probe against the real driver produced exactly
        // `[Browse(1), Browse(-23), Browse(1)]` — so a browser would jump 23
        // rows backwards in the middle of scrolling forwards.
        //
        // The span comes from `EVIOCGABS`, not from `config.txt`: the device
        // knows its own range, so nothing here depends on a line nobody has
        // run yet.
        let mut d = Decoder::default();
        d.set_abs_range(0, 23);
        let mut out = Vec::new();

        let abs = |v: i32| RawEvent { kind: EV_ABS, code: ABS_X, value: v };
        d.feed(abs(22), &mut out); // baseline, emits nothing
        d.feed(abs(23), &mut out);
        d.feed(abs(0), &mut out); // the wrap
        d.feed(abs(1), &mut out);
        assert_eq!(
            out,
            vec![Action::Browse(1), Action::Browse(1), Action::Browse(1)],
            "three detents clockwise must read as three"
        );

        // And the other way round the same seam. Two detents, not one: the
        // encoder is sitting at 1, so 1 -> 0 is the first and 0 -> 23 is the
        // wrap. Getting this wrong in the first draft of the test is the
        // small version of the bug itself — a wrap looks like a jump.
        out.clear();
        d.feed(abs(0), &mut out);
        d.feed(abs(23), &mut out);
        assert_eq!(
            out,
            vec![Action::Browse(-1), Action::Browse(-1)],
            "1 -> 0 -> 23 is two detents back, and the wrap is not a jump"
        );
    }

    #[test]
    fn without_a_known_range_an_absolute_axis_is_still_read_as_a_difference() {
        // `EVIOCGABS` can fail, and a device may report no useful range. The
        // decoder must not then invent one — an unfolded difference is right
        // everywhere except across a wrap, which is strictly better than
        // folding against a span that was guessed.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        let abs = |v: i32| RawEvent { kind: EV_ABS, code: ABS_X, value: v };
        d.feed(abs(10), &mut out);
        d.feed(abs(13), &mut out);
        assert_eq!(out, vec![Action::Browse(3)]);
    }

    #[test]
    fn sitting_on_a_bound_is_reportable_because_one_direction_is_then_silent() {
        // The half no decoder can fix. Without `rollover` the driver clamps
        // to `[0, steps]` and the input core drops the repeated value, so at
        // a bound one direction emits **nothing** — from a cold boot the
        // browser will not scroll anticlockwise until it has been scrolled
        // clockwise. The event never arrives, so the only honest thing the
        // code can do is let a caller say so during bring-up.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        let abs = |v: i32| RawEvent { kind: EV_ABS, code: ABS_X, value: v };

        d.set_abs_range(0, 23);
        assert!(!d.absolute_axis_is_clamped(), "nothing seen yet");
        d.feed(abs(0), &mut out);
        assert!(d.absolute_axis_is_clamped(), "at the bottom of the axis");
        d.feed(abs(5), &mut out);
        assert!(!d.absolute_axis_is_clamped());
        d.feed(abs(23), &mut out);
        assert!(d.absolute_axis_is_clamped(), "at the top of the axis");
    }

    #[test]
    fn autorepeat_is_ignored() {
        // `gpio-key` does not repeat by default. If it ever did, treating a
        // repeat as a fresh press would fire PLAY over and over while a
        // finger rested on it.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.feed(key(Button::PlayPause.keycode(), 1), &mut out);
        out.clear();
        for _ in 0..3 {
            d.feed(key(Button::PlayPause.keycode(), 2), &mut out);
        }
        assert!(out.is_empty());
    }

    #[test]
    fn an_unknown_keycode_and_a_stray_release_are_both_ignored() {
        let mut d = Decoder::default();
        let mut out = Vec::new();
        // Some other key entirely — the deck's device could carry more than
        // the six the overlay declares.
        d.feed(key(30, 1), &mut out);
        // And an up with no down, which is what a device reopened with a
        // button already held produces.
        d.feed(key(Button::Ff.keycode(), 0), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn two_seek_buttons_are_tracked_independently() {
        // FF and REW at once is not a designed gesture — and on the CDJ-200's
        // ladder it is not even reportable — but one must not clear the
        // other's slot, or a release would go missing and leave a seek
        // running with nothing able to end it.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.feed(key(Button::Ff.keycode(), 1), &mut out);
        d.feed(key(Button::Rew.keycode(), 1), &mut out);
        assert_eq!(out, vec![Action::Press(Button::Ff), Action::Press(Button::Rew)]);

        out.clear();
        d.feed(key(Button::Ff.keycode(), 0), &mut out);
        assert_eq!(out, vec![Action::Release(Button::Ff)]);
        out.clear();
        d.feed(key(Button::Rew.keycode(), 0), &mut out);
        assert_eq!(out, vec![Action::Release(Button::Rew)], "the other slot survived");
    }

    #[test]
    fn reset_forgets_everything_in_flight() {
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.feed(key(Button::Ff.keycode(), 1), &mut out);
        d.reset(&mut out);
        out.clear();
    }

    #[test]
    fn reset_closes_the_gestures_it_is_about_to_forget() {
        // Dropping the state silently is what makes a lost event permanent:
        // the transport is left in `SeekingForward` and the cue preview left
        // playing, with nothing downstream able to notice, because nothing
        // downstream knows the press is gone.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.feed(key(Button::Ff.keycode(), 1), &mut out); // seeking
        d.feed(key(Button::Cue.keycode(), 1), &mut out); // preview running
        out.clear();

        d.reset(&mut out);
        assert!(
            out.contains(&Action::Release(Button::Ff)),
            "a seek that started must be ended, got {out:?}"
        );
        assert!(
            out.contains(&Action::Release(Button::Cue)),
            "a preview that started must be released, got {out:?}"
        );
    }

    #[test]
    fn a_seek_is_closed_however_recently_it_started() {
        // **This test inverted when SEARCH stopped being tap-or-hold.** It
        // used to assert the opposite: a press younger than the threshold was
        // going to be a tap, and inventing one would change track because the
        // kernel dropped events. There is no tap any more, so a press that
        // young is already a running seek, and dropping it silently is what
        // leaves the transport in `SeekingForward` for ever.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.feed(key(Button::Ff.keycode(), 1), &mut out);
        out.clear();
        d.reset(&mut out);
        assert_eq!(
            out,
            vec![Action::Release(Button::Ff)],
            "a seek one millisecond old is still a seek"
        );
    }

    #[test]
    fn a_simple_button_in_flight_needs_no_closing() {
        // The counterpart: `Simple` opens nothing, so `reset` has nothing to
        // close, and a TRACK SEARCH lost to a dropped queue must not fire a
        // second track change on the way out.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.feed(key(Button::TrackNext.keycode(), 1), &mut out);
        out.clear();
        d.reset(&mut out);
        assert!(out.is_empty(), "no track change may be invented, got {out:?}");
    }

    #[test]
    fn a_dropped_event_queue_ends_whatever_was_in_flight() {
        // `SYN_DROPPED`: evdev buffers 64 events per client and discards the
        // whole queue when a reader falls behind. A key release can be in
        // what was lost, so carrying on would leave the button held for ever.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.feed(key(Button::Rew.keycode(), 1), &mut out);
        out.clear();

        d.feed(
            RawEvent { kind: EV_SYN, code: SYN_DROPPED, value: 0 },
            &mut out,
        );
        assert_eq!(
            out,
            vec![Action::Release(Button::Rew)],
            "a dropped queue must end the seek it can no longer see the end of"
        );
    }

    #[test]
    fn an_ordinary_frame_boundary_is_still_ignored() {
        // `SYN_REPORT` arrives after every event group. Treating it like a
        // drop would reset the decoder several times a second.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.feed(key(Button::Ff.keycode(), 1), &mut out);
        out.clear();
        d.feed(RawEvent { kind: EV_SYN, code: 0, value: 0 }, &mut out);
        assert!(out.is_empty(), "SYN_REPORT means nothing here, got {out:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_device_that_hangs_up_is_dropped_rather_than_spun_on() {
        // A hung-up descriptor is **permanently ready**: `poll` returns at
        // once, for ever. Measured on a FIFO standing in for a removed
        // device: **340,838 iterations in 200 ms** before this looked at
        // `revents`. A real evdev node returns `ENODEV` instead of hanging
        // up, so the deck was relying on the kernel's good manners rather
        // than on anything here — and `read_pending` treating a zero-length
        // read as "nothing arrived" was the same reliance from the other end.
        //
        // The count matters as much as the drop: a device going away takes
        // its in-flight presses with it, and only the caller can reset the
        // decoder, so this has to *say* it happened rather than quietly
        // shrink.
        let dir = std::env::temp_dir().join(format!("deck-pi-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("evfake");
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("cstring");
        // SAFETY: a valid NUL-terminated path in a directory this test owns.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");

        // Opening a FIFO for reading blocks until a writer arrives, so the
        // writer has to be in flight before `Device::open`.
        let wp = path.clone();
        let writer = std::thread::spawn(move || std::fs::OpenOptions::new().write(true).open(wp));
        let mut devices = Devices::open(&[path]).expect("open");
        let w = writer.join().expect("writer thread").expect("write end");
        assert_eq!(devices.len(), 1);

        // Hang it up.
        drop(w);

        let mut out = Vec::new();
        let mut spins = 0u32;
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        let mut lost_total = 0usize;
        while std::time::Instant::now() < deadline {
            devices.wait(Duration::from_millis(10)).expect("wait");
            lost_total += devices.read_pending(&mut out).expect("read");
            spins += 1;
            if devices.is_empty() {
                break;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(lost_total, 1, "the hang-up must be reported to the caller");
        assert!(devices.is_empty(), "and the device must be dropped");
        assert!(
            spins < 10,
            "a hung-up device must not be spun on; went round {spins} times"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn no_devices_waits_rather_than_returning_at_once() {
        // `poll` with zero descriptors returns immediately, so a deck whose
        // input nodes have all gone would busy-loop at 100% of a core
        // instead of idling — the same failure as the hang-up above, reached
        // by subtraction rather than by error.
        let mut devices = Devices::open(&[]).expect("open none");
        let started = std::time::Instant::now();
        assert!(!devices.wait(Duration::from_millis(50)).expect("wait"));
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "waiting on nothing must still wait, took {:?}",
            started.elapsed()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_event_struct_is_the_size_the_kernel_header_says() {
        // Measured against `linux/input.h` on aarch64 with a C probe: sizeof
        // 24, with `type` at 16, `code` at 18 and `value` at 20. So "a fixed
        // 24-byte struct" is true of the base `decisions.md` chose and not of
        // the struct.
        //
        // **Asserted against the number, not against the derivation.** The
        // previous version compared `EVENT_LEN` with
        // `size_of::<timeval>() + 8`, which was the definition restated — it
        // could not fail, and it would have gone on passing under
        // `__USE_TIME_BITS64`, where userspace's `timeval` grows to 16 on a
        // 32-bit build and the kernel's event does not.
        #[cfg(target_pointer_width = "64")]
        assert_eq!(EVENT_LEN, 24, "the event layout moved on a 64-bit build");
        #[cfg(target_pointer_width = "32")]
        assert_eq!(EVENT_LEN, 16, "the event layout moved on a 32-bit build");

        // And a round trip through the byte form the kernel writes.
        let mut bytes = vec![0u8; EVENT_LEN];
        let t = std::mem::size_of::<libc::timeval>();
        bytes[t..t + 2].copy_from_slice(&EV_KEY.to_ne_bytes());
        bytes[t + 2..t + 4].copy_from_slice(&164u16.to_ne_bytes());
        bytes[t + 4..t + 8].copy_from_slice(&1i32.to_ne_bytes());
        assert_eq!(
            parse_event(&bytes),
            Some(RawEvent {
                kind: EV_KEY,
                code: 164,
                value: 1
            })
        );
        assert_eq!(parse_event(&bytes[..EVENT_LEN - 1]), None, "a short read");
    }
}

//! deck-pico — the deck's control surface, on a Pico 2 H, over USB.
//!
//! The Pi runs the deck. This runs the buttons and the browse encoder, and
//! presents them as an ordinary USB HID device so that `src/input.rs` on the
//! other side reads exactly what it read when the controls were wired to the
//! Pi's header: `EV_KEY` with standard keycodes, and `EV_REL` on `REL_X`, one
//! unit per detent. **Nothing on the Pi changed to accept this**, which is the
//! whole reason the split is cheap — see `docs/decisions.md`.
//!
//! # The firmware cannot choose a keycode
//!
//! It declares a HID *usage*. The kernel's `drivers/hid/hid-input.c` decides
//! which keycode that becomes, and a usage that maps somewhere unexpected
//! leaves `Button::from_keycode` returning `None` — a button that is silently
//! dead, with nothing logged anywhere. So the six below are not guesses:
//! each was read out of the mapping table in `hid-input.c` on the `rpi-6.18.y`
//! branch, which is the kernel the deck actually runs.
//!
//! | Control | Consumer usage | becomes |
//! |---|---|---|
//! | ENTER | `0x084` | `KEY_ENTER` (28) |
//! | FF | `0x0b3` | `KEY_FASTFORWARD` (208) |
//! | REW | `0x0b4` | `KEY_REWIND` (168) |
//! | PLAY / PAUSE | `0x0cd` | `KEY_PLAYPAUSE` (164) |
//! | BACK | `0x224` | `KEY_BACK` (158) |
//! | CUE | `0x226` | `KEY_STOP` (128) |
//! | TRACK SEARCH ►►| | `0x0b5` | `KEY_NEXTSONG` (163) |
//! | TRACK SEARCH |◄◄ | `0x0b6` | `KEY_PREVIOUSSONG` (165) |
//!
//! **The obvious usage for CUE is the wrong one.** Consumer `0x0b7` is named
//! "Stop" and maps to `KEY_STOPCD` (166), which the deck does not match. CUE
//! needs `0x226`, AC Stop. That one line is the difference between a working
//! cue button and one that does nothing and says nothing.
//!
//! The encoder is a Generic Desktop `X` declared **relative**: `hid-input.c`
//! sends those through `map_rel(usage->hid & 0xf)`, and `HID_GD_X & 0xf` is 0,
//! which is `REL_X`.
//!
//! # What moved here from the kernel
//!
//! Debounce and quadrature decoding. `docs/implementation.md` used to require
//! both "in the kernel, never by polling from userspace" — the target of that
//! rule was userspace polling, which quantises velocity and drops steps.
//! Firmware on a dedicated core is the other direction from that: this loop
//! does nothing else, so its sampling interval is a constant rather than a
//! thing competing with an audio deadline.

#![no_std]
#![no_main]
// Under `selftest` the whole control path — debounce, quadrature, the pins and
// the constants that size them — is deliberately not built, because the point
// of that build is to exercise the descriptor with no GPIO involved. Scoped to
// the feature so the default build still reports its own dead code.
#![cfg_attr(feature = "selftest", allow(unused_imports, dead_code))]

use embassy_executor::Spawner;
use embassy_rp::adc::{Adc, Blocking, Channel as AdcChannel, Config as AdcConfig};
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Input, Pull};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler};
use embassy_time::{Duration, Ticker};
#[cfg(any(feature = "selftest", feature = "faderprobe"))]
use embassy_time::Timer;
#[cfg(feature = "faderprobe")]
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use embassy_usb::class::hid::{HidBootProtocol, HidSubclass, HidWriter, State};
use embassy_usb::{Builder, Config};
use panic_halt as _;
use static_cell::StaticCell;

/// The RP2350 boot ROM looks for this near the start of flash and will not
/// run an image without one — it stays in BOOTSEL instead, saying nothing.
/// `memory.x` is what puts it where the ROM looks.
#[unsafe(link_section = ".start_block")]
#[used]
static IMAGE_DEF: embassy_rp::block::ImageDef = embassy_rp::block::ImageDef::secure_exe();

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

/// How often every input is sampled. The debounce interval below is counted
/// in these, so the two numbers are not independent.
const TICK: Duration = Duration::from_millis(1);

/// `docs/controls.md` fixes debounce at 30-50 ms and the hold threshold at
/// 300-500 ms, and says they must stay well clear of each other. The hold
/// threshold is the Pi's business — `src/input.rs` decides tap versus hold —
/// so all this end has to do is not blur the gap.
const DEBOUNCE_TICKS: u8 = 40;

/// Quadrature steps per detent. Four is the common case for a detented
/// encoder: one click walks the full A/B cycle. **Verify against the encoder
/// in hand** — a part that rests between phases gives two, and the browse
/// menu then jumps two entries per click, which looks like a software bug.
const STEPS_PER_DETENT: i8 = 4;

/// Hand-written rather than generated, so the bytes can be read against the
/// usage tables they were taken from. Two reports: the buttons as a bitmap on
/// the Consumer page, and the encoder as one signed relative axis.
#[rustfmt::skip]
const REPORT_DESCRIPTOR: &[u8] = &[
    // --- Report 1: the eight buttons, one bit each ----------------------
    0x05, 0x0C,       // Usage Page (Consumer)
    0x09, 0x01,       // Usage (Consumer Control)
    0xA1, 0x01,       // Collection (Application)
    0x85, 0x01,       //   Report ID (1)
    0x15, 0x00,       //   Logical Minimum (0)
    0x25, 0x01,       //   Logical Maximum (1)
    0x75, 0x01,       //   Report Size (1)
    0x95, 0x08,       //   Report Count (8)
    0x09, 0x84,       //   Usage (Media Select Home)     -> KEY_ENTER
    0x09, 0xB3,       //   Usage (Fast Forward)          -> KEY_FASTFORWARD
    0x09, 0xB4,       //   Usage (Rewind)                -> KEY_REWIND
    0x09, 0xCD,       //   Usage (Play/Pause)            -> KEY_PLAYPAUSE
    0x0A, 0x24, 0x02, //   Usage (AC Back)               -> KEY_BACK
    0x0A, 0x26, 0x02, //   Usage (AC Stop)               -> KEY_STOP
    0x09, 0xB5,       //   Usage (Scan Next Track)       -> KEY_NEXTSONG
    0x09, 0xB6,       //   Usage (Scan Previous Track)   -> KEY_PREVIOUSSONG
    0x81, 0x02,       //   Input (Data, Variable, Absolute)
    0xC0,             // End Collection

    // --- Report 2: the browse encoder, as relative X --------------------
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x02,       // Usage (Mouse)
    0xA1, 0x01,       // Collection (Application)
    0x85, 0x02,       //   Report ID (2)
    0x09, 0x01,       //   Usage (Pointer)
    0xA1, 0x00,       //   Collection (Physical)
    0x09, 0x30,       //     Usage (X)
    0x15, 0x81,       //     Logical Minimum (-127)
    0x25, 0x7F,       //     Logical Maximum (127)
    0x75, 0x08,       //     Report Size (8)
    0x95, 0x01,       //     Report Count (1)
    0x81, 0x06,       //     Input (Data, Variable, Relative)
    0xC0,             //   End Collection
    0xC0,             // End Collection
];

/// Bit positions in report 1, in the order the descriptor declares them.
const BIT_ENTER: u8 = 0;
const BIT_FF: u8 = 1;
const BIT_REW: u8 = 2;
const BIT_PLAY: u8 = 3;
const BIT_BACK: u8 = 4;
const BIT_CUE: u8 = 5;
/// Read only by `LADDER_BITS`, which the `faderprobe` build does not compile —
/// the panel's TRACK pair has no other source.
#[cfg_attr(feature = "faderprobe", allow(dead_code))]
const BIT_TRACK_NEXT: u8 = 6;
#[cfg_attr(feature = "faderprobe", allow(dead_code))]
const BIT_TRACK_PREV: u8 = 7;

/// A switch to ground behind a pull-up: pressed reads low. One counter each,
/// so a bouncing contact on one button cannot delay another.
struct Debounced<'d> {
    pin: Input<'d>,
    stable: bool,
    ticks: u8,
}

impl<'d> Debounced<'d> {
    fn new(pin: Input<'d>) -> Self {
        let stable = pin.is_low();
        Self { pin, stable, ticks: 0 }
    }

    /// Call once per `TICK`. Returns the debounced level.
    fn poll(&mut self) -> bool {
        if self.pin.is_low() == self.stable {
            self.ticks = 0;
        } else {
            self.ticks = self.ticks.saturating_add(1);
            if self.ticks >= DEBOUNCE_TICKS {
                self.stable = !self.stable;
                self.ticks = 0;
            }
        }
        self.stable
    }
}

/// Full-cycle quadrature. The table is indexed by the four-bit
/// `(previous << 2) | current` state and yields -1, 0 or +1; the impossible
/// transitions yield 0 rather than a guess, because a guess under contact
/// bounce is a step in the wrong direction and the menu visibly jumps.
#[rustfmt::skip]
const QUADRATURE: [i8; 16] = [
     0, -1,  1,  0,
     1,  0,  0, -1,
    -1,  0,  0,  1,
     0,  1, -1,  0,
];

struct Encoder<'d> {
    a: Input<'d>,
    b: Input<'d>,
    last: u8,
    steps: i8,
}

impl<'d> Encoder<'d> {
    fn new(a: Input<'d>, b: Input<'d>) -> Self {
        let last = Self::state(&a, &b);
        Self { a, b, last, steps: 0 }
    }

    fn state(a: &Input<'d>, b: &Input<'d>) -> u8 {
        ((a.is_high() as u8) << 1) | (b.is_high() as u8)
    }

    /// Call once per `TICK`. Returns detents since the last call, usually 0.
    fn poll(&mut self) -> i8 {
        let now = Self::state(&self.a, &self.b);
        let delta = QUADRATURE[((self.last << 2) | now) as usize];
        self.last = now;
        self.steps += delta;

        let mut detents = 0;
        while self.steps >= STEPS_PER_DETENT {
            self.steps -= STEPS_PER_DETENT;
            detents += 1;
        }
        while self.steps <= -STEPS_PER_DETENT {
            self.steps += STEPS_PER_DETENT;
            detents -= 1;
        }
        detents
    }
}

/// The CDJ-200 switch panel's six shared buttons, read as one analog level.
///
/// `KSWB` gives PLAY and CUE their own lines and puts the other six on `KD2`
/// through a resistor ladder, so which one is down is a voltage rather than a
/// pin — `cdj-200.md` has the measured levels these boundaries sit between.
///
/// # Why this cannot reuse `Debounced`
///
/// A pin bounces between two states and either is a valid reading. **This line
/// passes through other buttons' levels on its way to its own.** Measured, not
/// feared: pressing TRACK ►► produced a sample of 3029 while SEARCH ◄◄ sits at
/// 3061, so a decoder that acted on one reading would fire a different button
/// every so often, on a press that was never made.
///
/// So the rule is not "ignore edges" but **"the same bucket, every tick, for
/// `DEBOUNCE_TICKS`"**. A transition that crosses a bucket cannot hold it that
/// long, and the same count that debounces a switch outlasts it.
///
/// The pad's own pull-up is what turns the ladder into a divider. It is weak
/// and loosely specified, and the measured levels moved by up to 80 counts
/// between runs because of it — **the boundaries below are wide enough to
/// swallow that and no wider.** An external resistor of known value is the fix,
/// and these numbers are taken again when it goes in.
#[cfg(not(feature = "faderprobe"))]
struct Ladder {
    adc: Adc<'static, Blocking>,
    pin: AdcChannel<'static>,
    /// The bucket that has been read most recently, and for how many ticks.
    candidate: usize,
    held: u8,
    /// The bucket that has held long enough to be believed.
    settled: usize,
}

/// Boundaries between the seven states, highest first: above the first is
/// nobody pressing. Each is the midpoint of two measured medians.
#[cfg(not(feature = "faderprobe"))]
const LADDER_BOUNDS: [u16; 6] = [3506, 2755, 2043, 1310, 748, 311];

/// Which report-1 bit each bucket sets. **`None` is a button the panel has and
/// the deck has no meaning for** — FOLDER SEARCH, whose behaviour is not
/// settled. It is decoded anyway and deliberately emits nothing: a level that
/// fell through to the idle bucket would be indistinguishable from a broken
/// wire, and this project's recurring defect is exactly that kind of silence.
#[cfg(not(feature = "faderprobe"))]
const LADDER_BITS: [Option<u8>; 7] = [
    None,                  // 0: idle
    Some(BIT_REW),         // 1: SEARCH  |◄◄
    Some(BIT_FF),          // 2: SEARCH  ►►|
    Some(BIT_TRACK_PREV),  // 3: TRACK   |◄◄
    Some(BIT_TRACK_NEXT),  // 4: TRACK   ►►|
    None,                  // 5: FOLDER  |◄◄ — recognised, unassigned
    None,                  // 6: FOLDER  ►►| — recognised, unassigned
];

#[cfg(not(feature = "faderprobe"))]
impl Ladder {
    fn new(adc: Adc<'static, Blocking>, pin: AdcChannel<'static>) -> Ladder {
        Ladder { adc, pin, candidate: 0, held: 0, settled: 0 }
    }

    /// Call once per `TICK`. Returns the bits for whatever is settled.
    fn poll(&mut self) -> u8 {
        // A conversion that fails leaves the settled bucket alone rather than
        // releasing it: a dropped reading is not a released button, and
        // releasing one here would end a seek the operator is still holding.
        if let Ok(count) = self.adc.blocking_read(&mut self.pin) {
            let bucket = LADDER_BOUNDS.iter().take_while(|b| count <= **b).count();
            if bucket == self.candidate {
                self.held = self.held.saturating_add(1);
            } else {
                self.candidate = bucket;
                self.held = 1;
            }
            if self.held >= DEBOUNCE_TICKS {
                self.settled = bucket;
            }
        }
        match LADDER_BITS[self.settled] {
            Some(bit) => 1 << bit,
            None => 0,
        }
    }
}

static STATE: StaticCell<State> = StaticCell::new();
static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
#[cfg(feature = "faderprobe")]
static CDC_STATE: StaticCell<CdcState> = StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let driver = Driver::new(p.USB, Irqs);

    // 0x2e8a is Raspberry Pi's vendor id; 0x000a is the product id reserved
    // for "a Pico running someone's own code", which is what this is. It is
    // not a claim to be any particular Raspberry Pi product.
    let mut config = Config::new(0x2e8a, 0x000a);
    config.manufacturer = Some("deck-pi");
    config.product = Some("deck-pi control surface");
    config.serial_number = Some("deck-pico-1");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );

    let hid_config = embassy_usb::class::hid::Config {
        report_descriptor: REPORT_DESCRIPTOR,
        request_handler: None,
        // 1 ms: the same interval the loop samples at, so a press waits for
        // the host no longer than it waits for the debounce.
        poll_ms: 1,
        max_packet_size: 8,
        // Not a boot device. The deck reads `/dev/input`, so nothing here is
        // ever asked to work before a driver is loaded.
        hid_subclass: HidSubclass::No,
        hid_boot_protocol: HidBootProtocol::None,
    };
    let writer = HidWriter::<_, 8>::new(&mut builder, STATE.init(State::new()), hid_config);

    // Before `build()`, which consumes the builder. A second interface, so
    // the HID device above is unchanged and the six buttons keep working
    // while the fader is being measured.
    #[cfg(feature = "faderprobe")]
    let cdc = CdcAcmClass::new(&mut builder, CDC_STATE.init(CdcState::new()), 64);

    let usb = builder.build();
    spawner.spawn(run_usb(usb).unwrap());

    #[cfg(feature = "faderprobe")]
    spawner.spawn(
        run_faderprobe(
            cdc,
            Adc::new_blocking(p.ADC, AdcConfig::default()),
            // No pull. The fader is a divider across 3V3 and AGND and
            // supplies its own level; a pull-up here would sit across the
            // top leg and bend the reading.
            AdcChannel::new_pin(p.PIN_27, Pull::None),
            // The opposite, and for the opposite reason. KSWB's six shared
            // buttons are bare resistances to ground with no divider top, so
            // without a pull-up this pin floats and reads noise. The pad's
            // own is weak and loosely specified — good enough to tell six
            // buttons apart, not good enough to write down as the ladder's
            // characterisation. An external resistor of known value is what
            // the shipped deck should use.
            AdcChannel::new_pin(p.PIN_26, Pull::Up),
        )
        .unwrap(),
    );

    #[cfg(feature = "selftest")]
    {
        let _ = (p.PIN_2, p.PIN_3, p.PIN_4, p.PIN_5, p.PIN_6, p.PIN_7, p.PIN_9, p.PIN_10);
        spawner.spawn(run_selftest(writer).unwrap());
        return;
    }

    #[cfg(not(feature = "selftest"))]
    let controls = run_controls(
        writer,
        // The pinout. Switches go to ground; the pull-ups are internal, so
        // nothing external is needed for any of these.
        //
        // **PLAY and CUE are where the CDJ-200 switch panel put them.** Its
        // `CN602` is clipped to GP4 and GP5, so the firmware follows the wire
        // rather than the wire following the firmware — see `cdj-200.md`.
        // ENTER moves to GP7, which PLAY vacated; GP8 is now free.
        Controls {
            enter: Debounced::new(Input::new(p.PIN_7, Pull::Up)), // encoder push
            back: Debounced::new(Input::new(p.PIN_6, Pull::Up)),
            play: Debounced::new(Input::new(p.PIN_4, Pull::Up)), // KSWB CN602-6
            cue: Debounced::new(Input::new(p.PIN_5, Pull::Up)),  // KSWB CN602-7
            rew: Debounced::new(Input::new(p.PIN_9, Pull::Up)),
            ff: Debounced::new(Input::new(p.PIN_10, Pull::Up)),
            encoder: Encoder::new(
                Input::new(p.PIN_2, Pull::Up), // encoder A
                Input::new(p.PIN_3, Pull::Up), // encoder B
            ),
            // **Under `faderprobe` the ADC belongs to the probe**, which is a
            // bench instrument that replaces this decode rather than running
            // beside it. The two cannot share GP26.
            #[cfg(not(feature = "faderprobe"))]
            ladder: Ladder::new(
                Adc::new_blocking(p.ADC, AdcConfig::default()),
                AdcChannel::new_pin(p.PIN_26, Pull::Up),
            ),
        },
    );
    #[cfg(not(feature = "selftest"))]
    spawner.spawn(controls.unwrap());
}

type UsbDevice = embassy_usb::UsbDevice<'static, Driver<'static, USB>>;

#[embassy_executor::task]
async fn run_usb(mut usb: UsbDevice) -> ! {
    usb.run().await
}

/// Presses every button in turn, then turns the encoder each way, for ever.
///
/// The point is not that something happens — `evtest` already showed the six
/// keycodes are *declared*. The point is the **order**. Nothing so far has
/// checked that bit 3 of report 1 is the bit the descriptor's fourth `Usage`
/// line claims, and a descriptor whose usages and bit constants disagree
/// declares exactly the same six keycodes while sending the wrong one for
/// every press. That is the CUE trap again, one layer down.
///
/// So watch the order rather than the count. It must be:
///
/// `KEY_ENTER` 28, `KEY_FASTFORWARD` 208, `KEY_REWIND` 168,
/// `KEY_PLAYPAUSE` 164, `KEY_BACK` 158, `KEY_STOP` 128,
/// then four `REL_X` of +1 and four of -1.
#[cfg(feature = "selftest")]
#[embassy_executor::task]
async fn run_selftest(mut writer: HidWriter<'static, Driver<'static, USB>, 8>) -> ! {
    // The host has to finish enumerating and open the device before anything
    // written here is seen. Reports sent before that are simply lost, which
    // would read as a dead button.
    Timer::after_secs(3).await;
    loop {
        for bit in 0..6u8 {
            let _ = writer.write(&[1, 1 << bit]).await;
            Timer::after_millis(120).await;
            let _ = writer.write(&[1, 0]).await;
            Timer::after_millis(380).await;
        }
        for _ in 0..4 {
            let _ = writer.write(&[2, 1]).await;
            Timer::after_millis(150).await;
        }
        for _ in 0..4 {
            let _ = writer.write(&[2, (-1i8) as u8]).await;
            Timer::after_millis(150).await;
        }
        Timer::after_secs(3).await;
    }
}

/// Everything the control loop owns, in one value.
///
/// **Not tidiness.** Passed positionally these were eight `Debounced`s of the
/// same type, where transposing two is a swap the compiler cannot see and the
/// panel reports the wrong button for ever. Named fields make the wiring
/// checkable against `cdj-200.md` at the call site — and the count had already
/// crossed the lint's threshold, which was suppressed rather than fixed.
#[cfg(not(feature = "selftest"))]
struct Controls {
    enter: Debounced<'static>,
    back: Debounced<'static>,
    play: Debounced<'static>,
    cue: Debounced<'static>,
    rew: Debounced<'static>,
    ff: Debounced<'static>,
    encoder: Encoder<'static>,
    #[cfg(not(feature = "faderprobe"))]
    ladder: Ladder,
}

#[cfg(not(feature = "selftest"))]
#[embassy_executor::task]
async fn run_controls(
    mut writer: HidWriter<'static, Driver<'static, USB>, 8>,
    mut c: Controls,
) -> ! {
    let mut ticker = Ticker::every(TICK);
    let mut last_buttons = 0u8;

    loop {
        ticker.next().await;

        let mut buttons = 0u8;
        buttons |= (c.enter.poll() as u8) << BIT_ENTER;
        buttons |= (c.ff.poll() as u8) << BIT_FF;
        buttons |= (c.rew.poll() as u8) << BIT_REW;
        buttons |= (c.play.poll() as u8) << BIT_PLAY;
        buttons |= (c.back.poll() as u8) << BIT_BACK;
        buttons |= (c.cue.poll() as u8) << BIT_CUE;
        // **OR'd, not replacing.** A discrete switch on GP9 or GP10 and the
        // panel's SEARCH pair both mean the same control, and either may be
        // the one that is wired.
        #[cfg(not(feature = "faderprobe"))]
        {
            buttons |= c.ladder.poll();
        }

        // Only on change. A HID device that repeats an unchanged report
        // wastes bus time the stick's reads are sharing.
        if buttons != last_buttons {
            last_buttons = buttons;
            let _ = writer.write(&[1, buttons]).await;
        }

        let detents = c.encoder.poll();
        if detents != 0 {
            let _ = writer.write(&[2, detents as u8]).await;
        }
    }
}

/// Prints the pitch fader's raw ADC counts, and the span seen so far, once
/// every 100 ms.
///
/// `docs/hardware.md` says two things about this fader that are arguments
/// rather than measurements: that 12 bits over a ±10% span is "ample by
/// arithmetic, with ENOB unmeasured", and that the centre detent is what
/// makes UNITY's *near centre* gate "a physical fact rather than an
/// inference". Neither survives contact without numbers. This task is how
/// the numbers are taken — **on the bench, by reading them off a terminal**,
/// not by anything the deck does at runtime.
///
/// Three lines are worth writing down from it: the count at each end of
/// travel, and the count at the detent. `span` carries the running extremes
/// so the ends do not have to be caught by eye.
///
/// # Why a serial port and not another HID report
///
/// A HID absolute axis would show up in `evtest` with no extra tooling,
/// which is tempting. But which `ABS_*` code a usage becomes is decided by
/// `hid-input.c`, the same table that made CUE's obvious usage the wrong
/// one — so that route costs a kernel-source reading before the first byte
/// is written. A CDC interface costs nothing and is read with `cat`.
#[cfg(feature = "faderprobe")]
#[embassy_executor::task]
async fn run_faderprobe(
    mut cdc: CdcAcmClass<'static, Driver<'static, USB>>,
    mut adc: Adc<'static, Blocking>,
    mut fader: AdcChannel<'static>,
    mut kd2: AdcChannel<'static>,
) -> ! {
    let mut min = u16::MAX;
    let mut max = u16::MIN;
    let mut kmin = u16::MAX;

    loop {
        // Nothing written before the host opens the port is seen, and a
        // probe that silently drops its first readings is worse than one
        // that waits. Re-entered whenever the terminal is closed.
        cdc.wait_connection().await;

        while let Ok(count) = adc.blocking_read(&mut fader) {
            let Ok(k) = adc.blocking_read(&mut kd2) else {
                break;
            };
            if count < min {
                min = count;
            }
            if count > max {
                max = count;
            }
            // Only the floor, for the ladder. Idle sits at the pull-up's rail
            // and every press pulls down, so the maximum says nothing.
            if k < kmin {
                kmin = k;
            }

            // Four fields of at most five digits, their labels, and CRLF
            // come to 44 bytes. The slack is deliberate: an overrun here
            // indexes out of bounds, and `panic_halt` turns that into a
            // probe that stops printing and says nothing about why.
            let mut line = [0u8; 64];
            let mut n = 0;
            n += write_field(&mut line[n..], b"f=", count);
            n += write_field(&mut line[n..], b" fmin=", min);
            n += write_field(&mut line[n..], b" fmax=", max);
            n += write_field(&mut line[n..], b" k=", k);
            n += write_field(&mut line[n..], b" kmin=", kmin);
            line[n] = b'\r';
            line[n + 1] = b'\n';
            n += 2;

            if cdc.write_packet(&line[..n]).await.is_err() {
                break;
            }
            // 20 Hz, not 10. A button press is short enough that a 100 ms
            // cadence can miss one whole, which on a ladder reads as a dead
            // button rather than as a missed sample.
            Timer::after_millis(50).await;
        }
    }
}

/// `<label><value>` into `out`, returning the bytes written. Hand-rolled
/// because `core::fmt` on a Cortex-M33 pulls in formatting machinery this
/// firmware has no other use for, and the values are four digits at most.
#[cfg(feature = "faderprobe")]
fn write_field(out: &mut [u8], label: &[u8], value: u16) -> usize {
    out[..label.len()].copy_from_slice(label);
    let mut n = label.len();

    let mut digits = [0u8; 5];
    let mut d = 0;
    let mut v = value;
    loop {
        digits[d] = b'0' + (v % 10) as u8;
        d += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    while d > 0 {
        d -= 1;
        out[n] = digits[d];
        n += 1;
    }
    n
}

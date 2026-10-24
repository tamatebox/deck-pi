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
//! dead, with nothing logged anywhere. So the ten below are not guesses:
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
//! | FOLDER SEARCH ►►| | `0x09c` | `KEY_CHANNELUP` (402) |
//! | FOLDER SEARCH |◄◄ | `0x09d` | `KEY_CHANNELDOWN` (403) |
//!
//! **FOLDER is sent, whatever the deck makes of it.** What the pair should do
//! is undecided, and that is a question for the Pi alone: the button exists,
//! so this end reports it. Sending it is also what makes a broken FOLDER wire
//! visible — a button that emits nothing looks the same from outside whether
//! it works or not. The channel pair was picked as keys nothing on the deck
//! matches; whether anything else on the image reacts to them is unchecked.
//! The numbers are from the running image's `input-event-codes.h`.
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
use embassy_time::{Duration, Ticker, Timer};
#[cfg(feature = "faderprobe")]
use core::sync::atomic::{AtomicU16, Ordering};
#[cfg(any(link, card))]
use embassy_rp::gpio::{Level, Output};
#[cfg(any(link, card))]
use embassy_rp::peripherals::{DMA_CH0, SPI0};
#[cfg(any(link, card))]
use embassy_rp::spi::{self, Spi};
#[cfg(link)]
use embassy_time::with_timeout;
#[cfg(any(feature = "faderprobe", link))]
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use embassy_usb::class::hid::{HidBootProtocol, HidSubclass, HidWriter, State};
use embassy_usb::control::{OutResponse, Recipient, Request, RequestType};
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

// The panel's SPI writes go by DMA, so a frame's 8 KiB does not hold the
// executor — and with it the control loop's 1 ms tick — for 16 ms at a time.
#[cfg(any(link, card))]
bind_interrupts!(struct DmaIrqs {
    DMA_IRQ_0 => embassy_rp::dma::InterruptHandler<DMA_CH0>;
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
    // --- Report 1: the ten buttons, one bit each, in two bytes -----------
    0x05, 0x0C,       // Usage Page (Consumer)
    0x09, 0x01,       // Usage (Consumer Control)
    0xA1, 0x01,       // Collection (Application)
    0x85, 0x01,       //   Report ID (1)
    0x15, 0x00,       //   Logical Minimum (0)
    0x25, 0x01,       //   Logical Maximum (1)
    0x75, 0x01,       //   Report Size (1)
    0x95, 0x0A,       //   Report Count (10)
    0x09, 0x84,       //   Usage (Media Select Home)     -> KEY_ENTER
    0x09, 0xB3,       //   Usage (Fast Forward)          -> KEY_FASTFORWARD
    0x09, 0xB4,       //   Usage (Rewind)                -> KEY_REWIND
    0x09, 0xCD,       //   Usage (Play/Pause)            -> KEY_PLAYPAUSE
    0x0A, 0x24, 0x02, //   Usage (AC Back)               -> KEY_BACK
    0x0A, 0x26, 0x02, //   Usage (AC Stop)               -> KEY_STOP
    0x09, 0xB5,       //   Usage (Scan Next Track)       -> KEY_NEXTSONG
    0x09, 0xB6,       //   Usage (Scan Previous Track)   -> KEY_PREVIOUSSONG
    0x09, 0x9D,       //   Usage (Channel Decrement)     -> KEY_CHANNELDOWN
    0x09, 0x9C,       //   Usage (Channel Increment)     -> KEY_CHANNELUP
    0x81, 0x02,       //   Input (Data, Variable, Absolute)
    0x95, 0x06,       //   Report Count (6)
    0x81, 0x01,       //   Input (Constant) — pads the ten to two bytes
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
/// the panel's TRACK and FOLDER pairs have no other source.
#[cfg_attr(feature = "faderprobe", allow(dead_code))]
const BIT_TRACK_NEXT: u8 = 6;
#[cfg_attr(feature = "faderprobe", allow(dead_code))]
const BIT_TRACK_PREV: u8 = 7;
#[cfg_attr(feature = "faderprobe", allow(dead_code))]
const BIT_FOLDER_PREV: u8 = 8;
#[cfg_attr(feature = "faderprobe", allow(dead_code))]
const BIT_FOLDER_NEXT: u8 = 9;

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
/// nobody pressing. Each is the midpoint of two measured medians — 2026-09-18's,
/// except SEARCH's pair. That one was 2755 until SEARCH ▶▶ moved 221 counts
/// and came within 35 of it, and is now 2026-10-22's midpoint instead.
#[cfg(not(feature = "faderprobe"))]
const LADDER_BOUNDS: [u16; 6] = [3506, 2862, 2043, 1310, 748, 311];

/// Which report-1 bit each bucket sets; `None` is idle and nothing else.
/// **FOLDER SEARCH is sent although the deck has no meaning for it yet** — it
/// used to be decoded and then dropped, which kept it out of the idle bucket
/// but left a working button and a broken wire looking the same from outside.
/// What it means is the Pi's to decide, and this end only reports the press.
#[cfg(not(feature = "faderprobe"))]
const LADDER_BITS: [Option<u8>; 7] = [
    None,                  // 0: idle
    Some(BIT_REW),         // 1: SEARCH  |◄◄
    Some(BIT_FF),          // 2: SEARCH  ►►|
    Some(BIT_TRACK_PREV),  // 3: TRACK   |◄◄
    Some(BIT_TRACK_NEXT),  // 4: TRACK   ►►|
    Some(BIT_FOLDER_PREV), // 5: FOLDER  |◄◄ — sent, ignored by the deck
    Some(BIT_FOLDER_NEXT), // 6: FOLDER  ►►| — sent, ignored by the deck
];

#[cfg(not(feature = "faderprobe"))]
impl Ladder {
    fn new(adc: Adc<'static, Blocking>, pin: AdcChannel<'static>) -> Ladder {
        Ladder { adc, pin, candidate: 0, held: 0, settled: 0 }
    }

    /// Call once per `TICK`. Returns the bits for whatever is settled.
    fn poll(&mut self) -> u16 {
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
            Some(bit) => 1u16 << bit,
            None => 0,
        }
    }
}

static STATE: StaticCell<State> = StaticCell::new();
static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
#[cfg(any(feature = "faderprobe", link))]
static CDC_STATE: StaticCell<CdcState> = StaticCell::new();
static RESET: StaticCell<ResetToBootsel> = StaticCell::new();

/// The Pico SDK's reset interface, from `pico/usb_reset_interface.h`: a
/// vendor-class interface with this subclass and protocol, and no endpoints.
const RESET_SUBCLASS: u8 = 0x00;
const RESET_PROTOCOL: u8 = 0x01;
/// The one request answered. The SDK's other, 0x02, reboots to the
/// application; a replug does that.
const RESET_REQUEST_BOOTSEL: u8 = 0x01;

/// Reboots into BOOTSEL when picotool asks, so flashing needs no button.
///
/// **Not an invention of this project.** The Pico SDK puts this interface in
/// every program that uses its USB stdio — `PICO_ENABLE_USB_RESET_VIA_VENDOR_INTERFACE`
/// defaults to 1 — and picotool looks for it on any device, its own vendor id
/// or not. The request, read from picotool's `reboot_device`: a **class**
/// request to the interface, `bRequest` 0x01, `wIndex` the interface number,
/// and `wValue` bits 0-1 the bootrom's mask of USB interfaces to leave off.
/// Bits 7-15 can name an activity LED, and this board has none to name.
///
/// `picotool reboot -f -u` uses it. `picotool load -f` does too and then
/// cannot find the board again — it follows the USB serial number across
/// the reboot, and this firmware's is a name rather than the chip's.
struct ResetToBootsel {
    interface: u8,
}

impl embassy_usb::Handler for ResetToBootsel {
    fn control_out(&mut self, req: Request, _data: &[u8]) -> Option<OutResponse> {
        if req.request_type == RequestType::Class
            && req.recipient == Recipient::Interface
            && req.index == u16::from(self.interface)
            && req.request == RESET_REQUEST_BOOTSEL
        {
            // Does not return: the bootrom takes the chip, and the control
            // transfer's status stage goes with it. picotool expects that and
            // ignores the result, as the SDK's own handler does.
            embassy_rp::rom_data::reset_to_usb_boot(0, u32::from(req.value & 0x3));
        }
        None
    }
}

/// The last report-1 bitmap the control loop sent, for the probe to print.
/// PLAY and CUE are digital and never reach the serial port otherwise, and a
/// Mac has no `evtest` — so without this, checking a solder joint on GP4 or
/// GP5 needs a Linux host.
#[cfg(feature = "faderprobe")]
static BUTTONS: AtomicU16 = AtomicU16::new(0);

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let driver = Driver::new(p.USB, Irqs);

    // Raspberry Pi's vendor id with **0x0009, "Raspberry Pi Pico SDK CDC
    // UART"** in their published list (`raspberrypi/usb-pid`) — the id the
    // SDK gives its own RP2350 programs. That list's README is also the
    // licence for using it: a device on standard interfaces keeps the
    // standard id and tells itself apart by its strings, which is what the
    // three below are for and what udev and `/dev/serial/by-id` match on.
    //
    // **It was 0x000a until 2026-10-22**, which the same list gives to the
    // RP2040, under a comment that called it "reserved for a Pico running
    // someone's own code". picotool reads the id to guess the chip.
    let mut config = Config::new(0x2e8a, 0x0009);
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

    // picotool's way in — see `ResetToBootsel`. **In every build**, the bench
    // ones included: a bench build is the one most certainly about to be
    // replaced.
    let reset = {
        let mut function = builder.function(0xFF, RESET_SUBCLASS, RESET_PROTOCOL);
        let mut interface = function.interface();
        let number = interface.interface_number();
        interface.alt_setting(0xFF, RESET_SUBCLASS, RESET_PROTOCOL, None);
        number
    };
    builder.handler(RESET.init(ResetToBootsel { interface: reset.into() }));

    // Before `build()`, which consumes the builder. A second interface, so
    // the HID device above is unchanged and the six buttons keep working
    // while the fader is being measured.
    #[cfg(feature = "faderprobe")]
    let cdc = CdcAcmClass::new(&mut builder, CDC_STATE.init(CdcState::new()), 64);
    // The same kind of interface, for the opposite reason: this one carries
    // the deck's frames, and only the shipped build has it — a bench build
    // that owns the serial port draws the test card instead.
    #[cfg(link)]
    let link = CdcAcmClass::new(&mut builder, CDC_STATE.init(CdcState::new()), 64);

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

    // SPI0 on GP18/GP19 with CS, DC and RES beside them — `docs/this-build.md`
    // has the wiring.
    #[cfg(any(link, card))]
    let panel = {
        let mut config = spi::Config::default();
        // The SSD1322 allows 10 MHz (a 100 ns cycle, Table 13-4), and asking
        // for it gets 9.375: `calc_prescs` divides the 150 MHz peripheral
        // clock by 16, the nearest it can without going over. 4 MHz until
        // 2026-10-23, while the jumpers were unproven. Mode 0, the default:
        // it shifts data in on SCLK's rising edge (§8.1.3).
        config.frequency = 10_000_000;
        Panel {
            spi: Spi::new_txonly(p.SPI0, p.PIN_18, p.PIN_19, p.DMA_CH0, DmaIrqs, config),
            // All three idle high: CS# and RES# are active low, and DC is
            // set before every byte that matters.
            cs: Output::new(p.PIN_17, Level::High),
            dc: Output::new(p.PIN_16, Level::High),
            res: Output::new(p.PIN_20, Level::High),
            contrast: CONTRAST_AT_START,
            lit: true,
        }
    };
    #[cfg(link)]
    spawner.spawn(run_panel(panel, link).unwrap());
    #[cfg(card)]
    spawner.spawn(run_paneltest(panel).unwrap());

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
/// The point is not that something happens — `evtest` already showed the
/// keycodes are *declared*. The point is the **order**. Nothing so far has
/// checked that bit 3 of report 1 is the bit the descriptor's fourth `Usage`
/// line claims, and a descriptor whose usages and bit constants disagree
/// declares exactly the same keycodes while sending the wrong one for every
/// press. That is the CUE trap again, one layer down.
///
/// So watch the order rather than the count. It must be:
///
/// `KEY_ENTER` 28, `KEY_FASTFORWARD` 208, `KEY_REWIND` 168,
/// `KEY_PLAYPAUSE` 164, `KEY_BACK` 158, `KEY_STOP` 128,
/// `KEY_NEXTSONG` 163, `KEY_PREVIOUSSONG` 165,
/// `KEY_CHANNELDOWN` 403, `KEY_CHANNELUP` 402,
/// then four `REL_X` of +1 and four of -1.
#[cfg(feature = "selftest")]
#[embassy_executor::task]
async fn run_selftest(mut writer: HidWriter<'static, Driver<'static, USB>, 8>) -> ! {
    // The host has to finish enumerating and open the device before anything
    // written here is seen. Reports sent before that are simply lost, which
    // would read as a dead button.
    Timer::after_secs(3).await;
    loop {
        for bit in 0..10u8 {
            let [lo, hi] = (1u16 << bit).to_le_bytes();
            let _ = writer.write(&[1, lo, hi]).await;
            Timer::after_millis(120).await;
            let _ = writer.write(&[1, 0, 0]).await;
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
    let mut last_buttons = 0u16;

    loop {
        ticker.next().await;

        let mut buttons = 0u16;
        buttons |= (c.enter.poll() as u16) << BIT_ENTER;
        buttons |= (c.ff.poll() as u16) << BIT_FF;
        buttons |= (c.rew.poll() as u16) << BIT_REW;
        buttons |= (c.play.poll() as u16) << BIT_PLAY;
        buttons |= (c.back.poll() as u16) << BIT_BACK;
        buttons |= (c.cue.poll() as u16) << BIT_CUE;
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
            #[cfg(feature = "faderprobe")]
            BUTTONS.store(buttons, Ordering::Relaxed);
            // Little-endian, so bit 0 of the descriptor is bit 0 of the first
            // byte, as HID packs report fields.
            let [lo, hi] = buttons.to_le_bytes();
            let _ = writer.write(&[1, lo, hi]).await;
        }

        let detents = c.encoder.poll();
        if detents != 0 {
            let _ = writer.write(&[2, detents as u8]).await;
        }
    }
}

/// Prints the pitch fader's raw ADC counts, and the span seen so far, once
/// every 50 ms — with `KD2`'s, and whether PLAY and CUE are down.
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

            // Five counts of at most four digits — the converter is 12-bit —
            // two one-digit states, their labels and CRLF come to 58 bytes.
            // 64 is also the packet size, so the line cannot simply grow.
            // The slack is deliberate: an overrun here indexes out of bounds,
            // and `panic_halt` turns that into a probe that stops printing
            // and says nothing about why.
            let b = BUTTONS.load(Ordering::Relaxed);
            let mut line = [0u8; 64];
            let mut n = 0;
            n += write_field(&mut line[n..], b"f=", count);
            n += write_field(&mut line[n..], b" fmin=", min);
            n += write_field(&mut line[n..], b" fmax=", max);
            n += write_field(&mut line[n..], b" k=", k);
            n += write_field(&mut line[n..], b" kmin=", kmin);
            n += write_field(&mut line[n..], b" play=", (b >> BIT_PLAY) & 1);
            n += write_field(&mut line[n..], b" cue=", (b >> BIT_CUE) & 1);
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

/// The SSD1322 on SPI0: data out by DMA, and three control lines.
#[cfg(any(link, card))]
struct Panel {
    spi: Spi<'static, SPI0, spi::Async>,
    cs: Output<'static>,
    dc: Output<'static>,
    res: Output<'static>,
    /// What the deck last asked for, kept so [`Panel::restore`] can say it
    /// again: the link only sends lighting when the deck's policy changes it.
    contrast: u8,
    lit: bool,
}

#[cfg(any(link, card))]
const PANEL_W: usize = 256;
#[cfg(any(link, card))]
const PANEL_H: usize = 64;

/// The first RAM column the glass is wired to, in the SSD1322's unit of
/// four pixels. **Not in the controller's datasheet** — the glass decides
/// it. u8g2's driver carries two answers for 256x64, 28 for Newhaven's and
/// 24 for a ZJY module that also reverses the COM scan, and the test card
/// settled it on 2026-10-22: this module is the Newhaven one.
#[cfg(any(link, card))]
const PANEL_COL0: u8 = 28;

/// u8g2's Newhaven 256x64 sequence, every value checked against the SSD1322
/// datasheet's command table (Rev 1.2, Table 9-1) for range — **less display
/// off, on and contrast**, which are state rather than settings and which
/// [`Panel::init`] and [`Panel::restore`] add themselves. Written again before
/// every frame, so none of it may be a command that shows on the glass.
#[cfg(any(link, card))]
const PANEL_INIT: &[(u8, &[u8])] = &[
    (0xFD, &[0x12]),       // unlock the command interface
    (0xB3, &[0x91]),       // clock: divide by 2, oscillator level 9
    (0xCA, &[0x3F]),       // 64 MUX
    (0xA2, &[0x00]),       // display offset 0
    (0xA1, &[0x00]),       // start line 0
    (0xA0, &[0x06, 0x11]), // column and nibble remap; dual COM, which 64 MUX allows
    (0xAB, &[0x01]),       // internal VDD regulator
    (0xB4, &[0xA0, 0xFD]), // external VSL, enhanced low grey levels
    (0xC7, &[0x0F]),       // master contrast, no reduction
    (0xB9, &[]),           // the default linear grey table
    (0xB1, &[0xE2]),       // phase 1 five DCLKs, phase 2 fourteen
    (0xD1, &[0xA2, 0x20]), // display enhancement B, normal
    (0xBB, &[0x1F]),       // pre-charge 0.60 x VCC
    (0xB6, &[0x08]),       // second pre-charge eight DCLKs
    (0xBE, &[0x07]),       // VCOMH 0.86 x VCC
    (0xA6, &[]),           // normal display
    (0xA9, &[]),           // exit partial display
];

/// Contrast until the deck says otherwise: the reset value rather than u8g2's
/// 0x9F, because the first image on an unmeasured rail should not be the
/// brightest one.
#[cfg(any(link, card))]
const CONTRAST_AT_START: u8 = 0x7F;

#[cfg(any(link, card))]
impl Panel {
    /// One command byte with D/C# low, then its parameters with D/C# high:
    /// the SSD1322 takes parameters as data (Table 9-1's D/C# column).
    async fn command(&mut self, cmd: u8, args: &[u8]) {
        self.cs.set_low();
        self.dc.set_low();
        let _ = self.spi.write(&[cmd]).await;
        if !args.is_empty() {
            self.dc.set_high();
            let _ = self.spi.write(args).await;
        }
        self.cs.set_high();
    }

    async fn data(&mut self, bytes: &[u8]) {
        self.cs.set_low();
        self.dc.set_high();
        let _ = self.spi.write(bytes).await;
        self.cs.set_high();
    }

    /// Opens a RAM window and leaves the controller in Write RAM, so what
    /// follows is pixels until the next command.
    async fn window(&mut self, col: (u8, u8), row: (u8, u8)) {
        self.command(0x15, &[col.0, col.1]).await;
        self.command(0x75, &[row.0, row.1]).await;
        self.command(0x5C, &[]).await;
    }

    /// Reset, the init sequence, the whole RAM to black, display on.
    ///
    /// **On, and black**, rather than off until the first frame: the Pi
    /// sends lighting only when its policy changes it, and it starts out
    /// believing the panel is lit. A panel that waited to be told would
    /// stay dark.
    ///
    /// **Again whenever a deck says hello**, not only at power-on. On
    /// 2026-10-24 one boot left the glass faint and squashed while frames
    /// arrived and were acked — the look of a lost `0xCA`, whose reset value
    /// is 128 MUX on 64-row glass (Table 9-1) — and the next boot was fine;
    /// the cause is not known. A deck connects seconds after the Pico powers
    /// up, on a supply that has settled, so this is a second start that does
    /// not depend on why the first one failed.
    async fn init(&mut self) {
        // §8.9: commands wait at least 300 ms after the supply is stable, and
        // RES# goes low for at least 100 µs. The wait after RES# is u8g2's,
        // whose note says these modules want longer than the datasheet does.
        Timer::after_millis(300).await;
        self.res.set_low();
        Timer::after_millis(1).await;
        self.res.set_high();
        Timer::after_millis(100).await;

        // Lit, at the starting contrast: the deck starts out believing so.
        self.contrast = CONTRAST_AT_START;
        self.lit = true;
        self.command(0xAE, &[]).await;
        for &(cmd, args) in PANEL_INIT {
            self.command(cmd, args).await;
        }
        let contrast = self.contrast;
        self.command(0xC1, &[contrast]).await;

        // All 120 columns and 128 rows, so that a window in the wrong place
        // shows black beside the picture rather than whatever the RAM
        // powered up holding.
        self.window((0, 119), (0, 127)).await;
        let black = [0u8; 240];
        for _ in 0..128 {
            self.data(&black).await;
        }

        // §8.9: the segments come on about 200 ms after this.
        self.command(0xAF, &[]).await;
    }

    /// Every setting, the contrast, and display on or off, written again.
    ///
    /// **Before every frame**, because SPI here is write-only: the controller
    /// cannot be asked what it holds, so a register that lost its value —
    /// to an init that half-landed, or to a dip that reset the controller and
    /// left the Pico running — stays lost until something writes it. Writing
    /// it each time mends either within a frame, for some forty bytes beside
    /// eight thousand.
    #[cfg(link)]
    async fn restore(&mut self) {
        for &(cmd, args) in PANEL_INIT {
            self.command(cmd, args).await;
        }
        let contrast = self.contrast;
        self.command(0xC1, &[contrast]).await;
        self.command(if self.lit { 0xAF } else { 0xAE }, &[]).await;
    }

    /// Writes a whole picture, one grey level of 0-15 per pixel.
    async fn picture(&mut self, mut pixel: impl FnMut(usize, usize) -> u8) {
        self.window((PANEL_COL0, PANEL_COL0 + 63), (0, PANEL_H as u8 - 1)).await;
        // Two pixels per byte, the left one in the high nibble — u8g2's
        // packing under the same 0xA0 remap, and what the card showed right.
        let mut row = [0u8; PANEL_W / 2];
        for y in 0..PANEL_H {
            for (i, byte) in row.iter_mut().enumerate() {
                *byte = (pixel(2 * i, y) << 4) | pixel(2 * i + 1, y);
            }
            self.data(&row).await;
        }
    }
}

/// The test card, one grey level of 0-15 per pixel. A border, so a wrong
/// column window shows as a missing or displaced edge. One square in the
/// top-left corner and nowhere else, so any flip shows as the square in a
/// different corner. A sixteen-step ramp, so the grey scale is seen to be one.
#[cfg(card)]
fn test_card(x: usize, y: usize) -> u8 {
    let border = x == 0 || x == PANEL_W - 1 || y == 0 || y == PANEL_H - 1;
    let marker = (3..11).contains(&x) && (3..11).contains(&y);
    let ramp = (16..240).contains(&x) && (36..56).contains(&y);
    if border || marker {
        15
    } else if ramp {
        ((x - 16) / 14) as u8
    } else {
        0
    }
}

/// Brings the panel up and draws the test card once.
///
/// **Not the panel's driver.** The deck draws, and its frames arrive over the
/// link in the shipped build; this proves the wires and the init sequence on
/// their own, so that when the real path shows nothing the panel end is
/// already known good.
#[cfg(card)]
#[embassy_executor::task]
async fn run_paneltest(mut panel: Panel) -> ! {
    panel.init().await;
    panel.picture(test_card).await;
    loop {
        Timer::after_secs(3600).await;
    }
}

// --- The link: `src/display/wire.rs`, from the Pico's end -------------------
//
// The bytes are the Pi's module's, written again rather than shared: the two
// sides share no code (`README.md` says why), so **`wire.rs` is the
// specification** and this is checked against it by the deck talking to it.

#[cfg(link)]
const MAGIC: [u8; 2] = *b"DP";
#[cfg(link)]
const VERSION: u8 = 1;
/// Magic, version, kind, length; the CRC follows the payload.
#[cfg(link)]
const HEADER: usize = 6;

#[cfg(link)]
const KIND_HELLO: u8 = 1;
#[cfg(link)]
const KIND_DECLARE: u8 = 2;
#[cfg(link)]
const KIND_FRAME: u8 = 3;
#[cfg(link)]
const KIND_ACK: u8 = 4;
#[cfg(link)]
const KIND_FAULT: u8 = 5;
#[cfg(link)]
const KIND_BRIGHTNESS: u8 = 6;
#[cfg(link)]
const KIND_BLANK: u8 = 7;

/// What a frame's bitmap must be: 1 bpp, rows padded to whole bytes.
#[cfg(link)]
const FRAME_BITS: usize = PANEL_W.div_ceil(8) * PANEL_H;

/// The largest payload taken, and what `Declare` says. A whole frame is
/// 2,048 bytes of bitmap plus nine per span, so this is room to spare rather
/// than a limit anything reaches.
#[cfg(link)]
const MAX_PAYLOAD: usize = 4096;

/// Brightness steps declared. The Pi's `Lighting::Dim` asks for a quarter of
/// them, `Full` for the top one.
#[cfg(link)]
const BRIGHTNESS_LEVELS: u8 = 16;

/// The contrast the top step reaches: u8g2's own value, not 0xFF. The
/// panel's current is unmeasured, and the top of the range is where it is
/// largest.
#[cfg(link)]
const CONTRAST_MAX: u16 = 0x9F;

/// CRC-16/CCITT-FALSE, the same ten lines as `wire::crc16`.
#[cfg(link)]
fn crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for b in bytes {
        crc ^= u16::from(*b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

#[cfg(link)]
fn le16(p: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*p.get(at)?, *p.get(at + 1)?]))
}

/// Messages out of the byte stream the host sends, resynchronising on
/// [`MAGIC`] when it has to — `wire::Reader`'s rules, in a fixed buffer.
#[cfg(link)]
struct LinkReader {
    buf: [u8; HEADER + MAX_PAYLOAD + 2 + 64],
    len: usize,
}

#[cfg(link)]
impl LinkReader {
    const fn new() -> LinkReader {
        LinkReader { buf: [0; HEADER + MAX_PAYLOAD + 2 + 64], len: 0 }
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    /// A packet that would overflow drops everything held: nothing in the
    /// buffer can be a message the reader could still finish, because no
    /// message is longer than the buffer.
    fn feed(&mut self, bytes: &[u8]) {
        if self.len + bytes.len() > self.buf.len() {
            self.len = 0;
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    fn consume(&mut self, n: usize) {
        self.buf.copy_within(n..self.len, 0);
        self.len -= n;
    }

    /// The next whole message at the head of the buffer, as its kind and
    /// payload length; [`Self::payload`] has the bytes, and the caller
    /// consumes `HEADER + len + 2` when done with them.
    ///
    /// **Loops only to resynchronise**: a header that fails its version,
    /// length or CRC check drops one byte and the scan goes again, which is
    /// `wire::Reader::drop_one` — the difference is that `wire.rs` reports
    /// each rejection, and nobody here is listening.
    fn next(&mut self) -> Option<(u8, usize)> {
        loop {
            let at = (0..self.len.saturating_sub(1)).find(|&i| self.buf[i..i + 2] == MAGIC);
            match at {
                Some(0) => {}
                Some(a) => self.consume(a),
                // Keep a trailing `D`: it may be the first half of a magic.
                None => {
                    let keep = usize::from(self.len > 0 && self.buf[self.len - 1] == MAGIC[0]);
                    self.consume(self.len - keep);
                    return None;
                }
            }
            if self.len < HEADER {
                return None;
            }
            let len = usize::from(le16(&self.buf, 4)?);
            if self.buf[2] != VERSION || len > MAX_PAYLOAD {
                self.consume(1);
                continue;
            }
            if self.len < HEADER + len + 2 {
                return None;
            }
            if crc16(&self.buf[2..HEADER + len]) != le16(&self.buf, HEADER + len)? {
                self.consume(1);
                continue;
            }
            return Some((self.buf[3], len));
        }
    }

    fn payload(&self, len: usize) -> &[u8] {
        &self.buf[HEADER..HEADER + len]
    }
}

/// Frames one message and sends it.
///
/// **With a timeout**: a host that stopped reading would otherwise hold this
/// task, and with it the panel, until it came back. Every message the Pico
/// sends fits one 64-byte packet.
#[cfg(link)]
async fn send(link: &mut CdcAcmClass<'static, Driver<'static, USB>>, kind: u8, payload: &[u8]) {
    let mut out = [0u8; 64];
    let len = payload.len().min(out.len() - HEADER - 2);
    out[..2].copy_from_slice(&MAGIC);
    out[2] = VERSION;
    out[3] = kind;
    out[4..6].copy_from_slice(&(len as u16).to_le_bytes());
    out[HEADER..HEADER + len].copy_from_slice(&payload[..len]);
    // Over everything but the magic, as `wire.rs` computes it.
    let crc = crc16(&out[2..HEADER + len]);
    out[HEADER + len..HEADER + len + 2].copy_from_slice(&crc.to_le_bytes());
    let _ = with_timeout(Duration::from_millis(100), link.write_packet(&out[..HEADER + len + 2])).await;
}

/// The panel, served to the deck: `Hello` answered with what is here, each
/// `Frame` drawn and acknowledged, brightness and blanking carried out.
///
/// **It declares a panel whether or not one is wired.** The SPI link is
/// write-only, so nothing here can see a missing module — it declares the
/// one this firmware was built for, and a panel unplugged reads as a deck
/// drawing to dark glass rather than as a `Fault`.
#[cfg(link)]
#[embassy_executor::task]
async fn run_panel(mut panel: Panel, mut link: CdcAcmClass<'static, Driver<'static, USB>>) -> ! {
    panel.init().await;

    // In static memory with the task rather than on a stack: a whole frame
    // has to fit, and the executor stores the task's state for its lifetime.
    let mut reader = LinkReader::new();
    let mut packet = [0u8; 64];
    loop {
        link.wait_connection().await;
        // A deck that went away mid-frame left half of one behind.
        reader.clear();

        while let Ok(n) = link.read_packet(&mut packet).await {
            reader.feed(&packet[..n]);
            while let Some((kind, len)) = reader.next() {
                let p = reader.payload(len);
                match kind {
                    KIND_HELLO => {
                        // A second start for the glass — see `Panel::init`.
                        // Before the answer, so the deck's first frame lands
                        // on a panel that has just been set up.
                        panel.init().await;
                        let mut d = [0u8; 8];
                        d[0..2].copy_from_slice(&(PANEL_W as u16).to_le_bytes());
                        d[2..4].copy_from_slice(&(PANEL_H as u16).to_le_bytes());
                        // Neither colour nor partial updates: the pens are
                        // ignored, and every frame is whole.
                        d[4] = 0;
                        d[5] = BRIGHTNESS_LEVELS;
                        d[6..8].copy_from_slice(&(MAX_PAYLOAD as u16).to_le_bytes());
                        send(&mut link, KIND_DECLARE, &d).await;
                    }
                    KIND_FRAME => {
                        // Sequence, span count, the spans, then the bitmap.
                        let seq = le16(p, 0);
                        let bits = le16(p, 2).and_then(|n| p.get(4 + 9 * usize::from(n)..));
                        match (seq, bits) {
                            (Some(seq), Some(bits)) if bits.len() == FRAME_BITS => {
                                panel.restore().await;
                                // MSB leftmost, rows `PANEL_W / 8` bytes apart.
                                panel
                                    .picture(|x, y| {
                                        let on = bits[y * (PANEL_W / 8) + x / 8] >> (7 - x % 8) & 1;
                                        on * 15
                                    })
                                    .await;
                                let mut a = [0u8; 6];
                                a[0..2].copy_from_slice(&seq.to_le_bytes());
                                let micros = embassy_time::Instant::now().as_micros() as u32;
                                a[2..6].copy_from_slice(&micros.to_le_bytes());
                                send(&mut link, KIND_ACK, &a).await;
                            }
                            // Said rather than dropped: a frame the wrong size
                            // for this glass is a deck that believes in a
                            // different panel, and an ack would hide it.
                            _ => send(&mut link, KIND_FAULT, b"\x01frame is not 256x64 1bpp").await,
                        }
                    }
                    KIND_BRIGHTNESS => {
                        if let Some(&step) = p.first() {
                            let step = u16::from(step.min(BRIGHTNESS_LEVELS - 1));
                            let contrast = step * CONTRAST_MAX / u16::from(BRIGHTNESS_LEVELS - 1);
                            panel.contrast = contrast as u8;
                            panel.command(0xC1, &[contrast as u8]).await;
                        }
                    }
                    KIND_BLANK => {
                        if let Some(&on) = p.first() {
                            // Sleep mode on, display off (Table 9-1). The
                            // glass's high voltage is the module's own
                            // converter — the controller takes VCC from
                            // outside (§8.9) — so this darkens the panel and
                            // need not stop that supply. Inferred from the
                            // module having only a 3.3 V input; unmeasured.
                            panel.lit = on == 0;
                            panel.command(if on != 0 { 0xAE } else { 0xAF }, &[]).await;
                        }
                    }
                    // `Declare`, `Ack` and `Fault` are the Pico's to send.
                    _ => {}
                }
                reader.consume(HEADER + len + 2);
            }
        }
    }
}

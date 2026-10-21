# This build

What is actually here, wired how. **Every other document is written to hold for any
build of this deck; this one is not.** It is the one place that says which parts
exist and what is connected, `CLAUDE.md` imports it so that every session starts
with it, and it changes in the same commit as anything that changes the bench.

Every fact is **supplied by the user** unless it says otherwise, and carries the date
it was true. A stale line here is a wrong fact, not an out-of-date opinion — date what
you change, and when a line's date is old and the question matters, ask.

## The boards

| | | |
|---|---|---|
| Pi | A dedicated Raspberry Pi 4, **not yet set up** | 2026-10-21 |
| — stand-in | `phono`, the user's vinyl-ripping Pi 4 (Rev 1.5, 2 GB). The Digi2 Pro is fitted to it for now; it is not the deck | 2026-09-24 |
| Digi2 Pro | Board release 2.2, mounted directly on the header | 2026-09-24 |
| IsolatorPi III | **Not fitted** | 2026-09-15 |
| Pico 2 H | Runs `firmware/`, reaches the Pi over USB | 2026-09-15 |

## The controls

| Control | Part | Pico | State |
|---|---|---|---|
| PLAY | CDJ-200 `KSWB`, CN602-6 | GP4 | **soldered** 2026-10-21 |
| CUE | `KSWB`, CN602-7 | GP5 | **soldered** 2026-10-21 |
| SEARCH, TRACK SEARCH, FOLDER SEARCH — two each | `KSWB` ladder, CN602-8 `KD2` | GP26 / ADC0, the pad's pull-up and **no external resistor** | **soldered** 2026-10-21 |
| Browse encoder, push = ENTER | **not bought** | GP2, GP3, GP7 held for it | 2026-10-21 |
| BACK | **not bought** | GP6 held for it | 2026-10-21 |
| Pitch fader — v2 | CDJ-200 `SLDB` | GP27 / ADC1 | test clips |
| Jog — v2 | CDJ-200 `JOGB` | undecided | unconnected |

The connectors pin by pin, and every measurement taken through them, are in
`cdj-200.md`.

**Soldered is not re-checked.** The joints replaced the test clips the measurements
were taken through, and nobody has yet confirmed that each wire landed on the GP the
table says. The ladder's thresholds in `firmware/src/main.rs` were set with the same
pad pull-up and no resistor went in, so they should still hold — **inferred, not
measured**. `faderprobe` prints `KD2`'s raw count and is the check.

**GP9 and GP10 are read, and nothing is on them.** The firmware ORs a discrete SEARCH
pair on those pins with the ladder's; this build has none and is not getting one
(2026-10-21). Pulled up, they idle released.

**Nothing turns a detent yet**, because the encoder is not bought — so `REL_X` has
been declared to the kernel and never sent by a real knob.

## The panel

An **SSD1322 3.12" 256x64 OLED** module, sold by WAVGAT. **Owned, not wired**
(2026-10-21). That answers [#2](https://github.com/tamatebox/deck-pi/issues/2) by
purchase rather than by its comparison.

- **Interface select is unread.** The SSD1322 chooses parallel or 4-wire SPI on two
  pins, `BS0` and `BS1` — 4-wire SPI is both low, per Orient Display's datasheet for
  its own SSD1322 module. Which way *this* board ships, and where it is set, comes off
  the board itself; no document for it has been found.
- **Power from the Pico's 3V3 is the plan.** The Pico 2 datasheet says the pin "can be
  used to power external circuitry" and recommends under **300 mA** (§3.1). Two things
  are owed before it is relied on: the panel's own draw at full white, **unmeasured**,
  and its effect on the ADC, which reads against that same rail filtered through
  201 Ω into 2.2 µF (§5.3). `faderprobe` with the panel blank and then full white,
  against the 2026-09-18 figures in `cdj-200.md`, is that measurement. GPIO23 high
  forces the regulator into PWM and cuts its ripple, if it comes to that.
- **Pins, proposed and not wired:** SPI0 — SCK GP18, MOSI GP19, CS GP17, DC GP16,
  RES GP20. Read against `embassy-rp` 0.10's pin table, not recalled.

## Pins on the Pico

| | |
|---|---|
| In use | GP4, GP5, GP26 (`KSWB`), GP27 (fader) |
| Held for parts not bought | GP2, GP3, GP6, GP7 |
| Read, nothing attached | GP9, GP10 |
| Proposed for the panel | GP16, GP17, GP18, GP19, GP20 |
| Free | GP0, GP1, GP8, GP11-GP15, GP21, GP22, GP28 |

## The medium

One real stick, mounted read-only at `/media/stick`: **2192 names** (2026-09-16), and
**no 192 kHz material**, which is why `tests/deck_soak.rs` has only run at 44.1 kHz.

## The bench

- **Builds happen on a Mac.** Linux binaries are built there in Docker
  (`rust:slim-trixie` with `pkg-config libsndfile1-dev libasound2-dev`) and copied to
  the Pi, which also makes the container the Linux gate when no Pi is reachable.
- **Every flash needs BOOTSEL held by hand while replugging.** The firmware exposes no
  reset interface, so `picotool load -f` cannot do it. `picotool` wants `-t elf`, the
  ELF having no extension.
- **`phono` is someone's working machine first.** `sudo` asks for a password, a
  capture service runs on it, and the user may be logged in — do not reboot it unasked.

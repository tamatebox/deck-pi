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
| PLAY | CDJ-200 `KSWB`, CN602-6 | GP4, pin 6 | jumper wire; **reads** 2026-10-22 |
| CUE | `KSWB`, CN602-7 | GP5, pin 7 | jumper wire; **reads** 2026-10-22 |
| SEARCH, TRACK SEARCH, FOLDER SEARCH — two each | `KSWB` ladder, CN602-8 `KD2` | GP26 / ADC0, pin 31, the pad's pull-up and **no external resistor** | jumper wire; **all six read** 2026-10-22 |
| Their ground | `KSWB`, CN602-9 `GNDA` | GND, pin 3 | jumper wire |
| Browse encoder, push = ENTER | **not bought** | GP2, GP3, GP7 held for it | 2026-10-21 |
| BACK | **not bought** | GP6 held for it | 2026-10-21 |
| Pitch fader — v2 | CDJ-200 `SLDB` | GP27 / ADC1 held for it | **unconnected** 2026-10-22 — the Pico has one 3V3 pin and the panel has it |
| Jog — v2 | CDJ-200 `JOGB` | undecided | unconnected |

The connectors pin by pin, and every measurement taken through them, are in
`cdj-200.md`.

**`KSWB` takes four wires and no supply.** `CN602-3` (`V+5V`) and the LED pins 1, 2,
4 and 5 are left off: the buttons only ever pull to ground, and the pull-ups are the
Pico's (2026-10-22).

**Checked through `faderprobe`, from a Mac, 2026-10-22.** PLAY and CUE each read
three presses as three clean edges — after a first try in which PLAY read as CUE
and CUE as nothing, fixed by moving the jumpers. The ladder gave six distinct
plateaus, each inside its own bucket, and idle sat at 3952 as it did on
2026-09-18. **Two of the six levels moved by about 200 counts since then, and one
came within 35 counts of its boundary**, which moved to 2862 the same day —
`cdj-200.md` has the numbers. The jumpers are soldered at the `KSWB` end and
plugged onto the Pico's header.

**And through `phono`'s kernel the same day**: all eight buttons pressed as their
own keycodes, FOLDER's 403 and 402 included.

**GP9 and GP10 are read, and nothing is on them.** The firmware ORs a discrete SEARCH
pair on those pins with the ladder's; this build has none and is not getting one
(2026-10-21). Pulled up, they idle released.

**Nothing turns a detent yet**, because the encoder is not bought — so `REL_X` has
been declared to the kernel and never sent by a real knob.

**Until it is, FOLDER ►►| is ENTER** (2026-10-22, `src/input.rs`). With no encoder
the selection never leaves the top row, so on a flat folder it loads the first file,
TRACK SEARCH walks from there and PLAY plays. **And the selection follows TRACK
SEARCH** in the folder shown (`src/app/deck.rs`), or the highlighted row stays the
first file and the playing track is named nowhere. Both are temporary and go when
the encoder is wired.

## The panel

An **SSD1322 3.12" 256x64 OLED** module, sold by WAVGAT, **blue** — its grey levels
are shades of blue. **Wired and showing the `paneltest` card correctly** on
2026-10-22: border on all four edges, the corner marker where it was drawn, and
the ramp in steps, at 4 MHz over jumper wires from the Pico's 3V3. That settles the
two things the controller's datasheet cannot: the glass starts at **RAM column 28**
and wants **`0xA0` = `0x06, 0x11`** — u8g2's Newhaven values, not its ZJY ones.
That answers [#2](https://github.com/tamatebox/deck-pi/issues/2) by purchase rather
than by its comparison.

**The deck draws on it** (2026-10-22): the shipped firmware serves it over USB, and
the deck on `phono` found the link, was declared 256x64, and put `No USB` on the
glass.

- **The header is seven pins, SPI only:** `GND VCC SCK SDA RES DC CS` (2026-10-22).
  The SSD1322 chooses parallel or 4-wire SPI on `BS0` and `BS1` — both low for
  4-wire SPI, per Orient Display's datasheet for its own SSD1322 module — and this
  board brings out no parallel data lines at all, so it is set for 4-wire SPI.
  **Inferred from the header**, not read off the select pins. `SDA` is SPI data in,
  not I2C.
- **It runs from the Pico's 3V3**, so 3.3 V is enough for it. The Pico 2 datasheet says the pin "can be
  used to power external circuitry" and recommends under **300 mA** (§3.1). Two things
  are owed before it is relied on: the panel's own draw at full white, **unmeasured**,
  and its effect on the ADC, which reads against that same rail filtered through
  201 Ω into 2.2 µF (§5.3). `faderprobe` with the panel blank and then full white,
  against the 2026-09-18 figures in `cdj-200.md`, is that measurement. GPIO23 high
  forces the regulator into PWM and cuts its ripple, if it comes to that.
- **Pins, wired 2026-10-22:** SPI0 — `SCK` GP18 (pin 24), `SDA` GP19 (25),
  `CS` GP17 (22), `DC` GP16 (21), `RES` GP20 (26), `GND` pin 23, `VCC` 3V3 (36).
  The SPI functions were read against `embassy-rp` 0.10's pin table, not recalled.

## Pins on the Pico

| | |
|---|---|
| In use | GP4, GP5, GP26 (`KSWB`) |
| Held for parts not bought | GP2, GP3, GP6, GP7 |
| Held for a part not connected | GP27 (fader) |
| Read, nothing attached | GP9, GP10 |
| The panel | GP16, GP17, GP18, GP19, GP20 |
| Free | GP0, GP1, GP8, GP11-GP15, GP21, GP22, GP28 |

## The medium

One real stick, mounted read-only at `/media/stick`: **2192 names** (2026-09-16), and
**no 192 kHz material**, which is why `tests/deck_soak.rs` has only run at 44.1 kHz.

## The bench

- **Builds happen on a Mac.** Linux binaries are built there in Docker
  (`rust:slim-trixie` with `pkg-config libsndfile1-dev libasound2-dev`) and copied to
  the Pi, which also makes the container the Linux gate when no Pi is reachable.
- **The Pico is flashed from `phono`, over ssh, with no button and no `sudo`**
  (2026-10-22): `picotool reboot -f -u`, then `picotool load -x -t elf
  ~/deck-bin/deck-pico`. The ELF is built on the Mac and copied there; `picotool`
  wants `-t elf`, the ELF having no extension.
- **Hold one ssh connection open to `phono`** (`ssh -M`) and run through it. On
  2026-10-22 its sshd began resetting every new connection — the user's too — just
  after a Pico was replugged, until it was power-cycled. The cause is unknown: its
  journal does not survive a reboot.
- **`phono:~/music` is the listening harness's, not a medium** — the deck reads only
  a mount point, and a folder on the SD card is not one (`src/media.rs`). Its
  `CD_BURN` folder holds 35 AIFFs from the CD_BURN tracklists, bit-identical to
  their ALAC sources (2026-10-22). **`phono:~/deck-bin/run-sd.sh` runs the deck on
  that folder anyway, without `sudo`**: inside `unshare --user --map-root-user
  --mount` it mounts an empty tmpfs and bind-mounts the folder read-only beneath it,
  so the mount point's device differs from its parent's. Temporary, like the
  FOLDER mapping above, and the listening harness has to be stopped first — both
  want the Digi2 Pro.
- **`phono` is someone's working machine first.** `sudo` asks for a password, a
  capture service runs on it, and the user may be logged in — do not reboot it unasked.

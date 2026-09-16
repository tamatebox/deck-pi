# deck-pi

A bit-perfect single-deck DJ transport on a Raspberry Pi 3B+. It reads a USB stick,
browses it by folder, and plays WAV/AIFF out as S/PDIF with the source samples
reaching the DAC untouched.

```
USB stick — exFAT or HFS+, mounted read-only
   |  folder tree is the index; no import step, no database
libsndfile, in the window thread     <- byte swap, 24-bit unpack, no deadline
   |
locked int32 ring                    <- the audio callback reads only this
   |  ALSA hw:, output rate matched to the file
Pi 3B+  --I2S-->  Digi2 Pro  --S/PDIF-->  DAC
   |  USB
Pico 2 H  <- buttons, browse encoder, panel
```

One Pi is one deck, which is what makes it safe to reopen the audio device for every
track — and that is what lets each track play at its own sample rate. Operation is
entirely network-independent: the stick is prepared elsewhere and carried, and the
Ethernet cable can be out during a set.

## Status

**The transport has run on hardware** — 2026-09-15, a Pi 3B+ with a Digi2 Pro
mounted directly, no isolator. Real tracks play out of S/PDIF at every rate in scope:
the device is opened at each track's own rate, the driver selects the right
oscillator on every change, frame counts come back exact and no run underran. Hearing
a track proves the transport and nothing about the samples — `docs/implementation.md`
says what does.

A real stick works end to end the same day: plugged into any port it mounts read-only
at the fixed path on its own, the browser lists it, and a track plays off it. Getting
there needed one fix — the drafted udev rule matched nothing, for a reason worth
reading before writing another one.

The Pico's firmware **enumerates on the Pi and declares the right things**: the
kernel registers keycodes 28, 128, 158, 164, 168 and 208 — exactly the six
`src/input.rs` matches — and a relative `REL_X` for the detents. That is the
declaration, read back from the kernel's own capability bitmaps. **No switch has
been wired to it yet**, so nothing has been pressed and no detent has been turned.

Worth knowing before sizing anything: **192 kHz at a 1.33 ms period ran clean on the
ordinary scheduler, with no realtime privileges** — and with nothing else on the
machine. The realtime setup passes separately. Read that as a floor, not as a verdict
on either: the display, the browser, media watch and the input loop were not running,
and `docs/implementation.md` is blunt that an idle desk plays fine with all three
realtime calls failing.

The v1 read path is built and tested — FFI, format vetting, the locked ring, the
window thread, the transport, the callback, the ALSA sink, the realtime setup, the
browser, media watch, the cue store and input — with bit-perfection verified end to
end at both depths and all six rates, across four of the five containers in scope
(**Wave64 is accepted and untested**).

The app loop is partly built: the period loop, the track lifecycle, the dispatch and
the control loop that reads `/dev/input`. The display is built in both halves:
`src/display.rs` decides what text goes in which cell, and `src/display/paint.rs`
draws it against the real 12 and 16 px Japanese faces — about 0.3 ms for a full
128x64 frame on the Pi, which `cargo test --release --test render_bench -- --ignored
--nocapture` prints. Still missing are **media watch wired to the deck**, and
the USB packer that carries those pixels to the Pico. `src/main.rs` is a bring-up
CLI, not the deck.

## What plays

Exactly what the DDC can send, so there is no second rule to remember.

| | |
|---|---|
| Container | WAV, AIFF, AIFF-C, RF64, Wave64 |
| Sample rate | 44.1 / 88.2 / 176.4 and 48 / 96 / 192 kHz |
| Bit depth | int16 and int24 only |

Every supported conversion is a pure shift, which is what makes v1's bit-perfection
*unconditional*. 32-bit float would need a clipping or scaling decision, and scaling
is a gain stage — so it is out of scope rather than handled. Anything refused is
converted when preparing the stick.

Every rejection is decidable from the header, so the browser refuses on **highlight**
rather than on PLAY. Nothing surprises you mid-set.

## Hardware

| | |
|---|---|
| Raspberry Pi 3B+ | 1 GB, Cortex-A53 — sized against **1.2 GHz**, not the headline 1.4 |
| HiFiBerry Digi2 Pro | WM8804, dual-domain clock, no volume control by design |
| IsolatorPi III | **optional, not fitted** — 5 kV galvanic isolation between the Pi and the audio boards |
| Pico 2 H | carries the buttons, the browse encoder and the panel; reaches the Pi over USB |

The Digi2 Pro carries separate oscillators for the 44.1 and 48 kHz families and runs
as clock master, so both come out of an exact crystal rather than the Pi's fractional
PLL — with or without an isolator, which is why fitting one is an improvement rather
than a requirement. It would keep the Pi's ground noise off the audio boards; it would
also offer the controls a header of their own, which this build has no use for now
that they are on the Pico. The 3B+ soft-throttles to 1.2 GHz at
60 °C by design, and a deck runs a continuous load inside a box, so the headline
clock is a sprint clock.

One assembly step is easy to get wrong and produces **no error when wrong**: leaving
GPIO 5/6 free, which the machine driver uses to pick the oscillator. Two more join it
if an isolator is ever fitted — the master/slave jumpers and the clean-side power
feed — and both live in [docs/hardware.md](docs/hardware.md).

## Controls

A detented rotary encoder for browsing, plus BACK, PLAY/PAUSE, CUE/STOP, and FF and
REW — hold to seek, tap to change track. ENTER is the encoder's push. The display
shows the browser and the transport. It also shows the rate and depth in use, and
**that is not how the chain is confirmed** — the panel reports what the deck believes,
so a deck wrong about its own output would be wrong on the panel in the same way. The
confirmation is `deck-pi --device=`, which reads `hw_params` back out of
`/proc/asound` and asks whether the mixer is empty: a different source, which is what
makes it a check.

Seven switches, two encoders and a fader, the v2 unity button being the seventh.
**None of them is on the Pi.** They hang off a Pico 2 H which reaches the Pi as a USB
device, so the deck reads the same `/dev/input` keycodes it always did and the Pi's
header carries nothing but audio. The panel went the same way, with the deck still
doing the drawing and shipping pixels over the link. Seeking is silent in v1: an
audible scan needs the resampler, which would give v1 a second mode. Buttons are
debounced and detents decoded on the Pico, never polled from userspace.

## Track length

Bounded by the **source container**, not by RAM — the window thread keeps only a
region around the playhead resident, so a three-hour file costs the same memory as a
three-minute one. RIFF and AIFF chunk sizes are 32-bit and widely treated as signed,
so 2 GiB is the compatible ceiling:

| | int16 | int24 |
|---|---|---|
| 44.1 kHz | 3 h 23 m | 2 h 15 m |
| 48 kHz | 3 h 06 m | 2 h 04 m |
| 96 kHz | 1 h 33 m | 1 h 02 m |
| 192 kHz | 47 m | 31 m |

Longer needs RF64 or Wave64, which libsndfile reads at no extra cost — so long-form
work at high rates is a container choice, not a limit.

## Building and running it

**Two binaries, on two machines, built differently.** The deck is `src/`, a hosted
Rust binary on the Pi. The control surface is `firmware/`, a `no_std` binary on a
Pico 2 H. They share no code and need different toolchains, so they are separated
below rather than left to be sorted out per command.

### A development machine — macOS or Linux

Everything except the hardware-facing half compiles and tests here.

```sh
brew install libsndfile pkg-config      # macOS; on Debian see the Pi block below
cargo test
```

### The Pi — the deck

libsndfile and ALSA are both system libraries found through `pkg-config`, and a
fresh Raspberry Pi OS Lite image has neither.

```sh
sudo apt install -y build-essential pkg-config libsndfile1-dev libasound2-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
cargo build --release
```

**That is the build, not the setup.** A fresh image also needs the realtime limits
and the stick's udev rule, which take effect only after a re-login and a re-plug
respectively — `docs/implementation.md`, *First boot, in order*, has them in the
order that avoids discovering that late.

### The Pico — the control surface

Cross-compiled; nothing about it is hosted, and it never runs on the Pi.

```sh
rustup target add thumbv8m.main-none-eabihf   # RP2350 is a Cortex-M33
cd firmware && cargo build --release
```

Flashing needs `picotool` on **whichever machine the Pico is plugged into**, which
during bring-up is the Pi rather than the development machine:

```sh
sudo apt install -y picotool                  # on the Pi
brew install picotool                         # on macOS
sudo picotool load -x target/thumbv8m.main-none-eabihf/release/deck-pico
```

Hold BOOTSEL while plugging the Pico in, or it is already running and will not
accept a load. `picotool` takes the ELF directly, so there is no UF2 step —
worth knowing because the common UF2 tooling still tags images with the RP2040
family id, which an RP2350 will not accept.

**Linux/aarch64 is the gate, and a green macOS run proves nothing about the deck** —
what compiles out on a Mac is precisely the hardware-facing half. Report the Linux
figure. `cargo clippy --all-targets -- -D warnings`, after
`rustup component add clippy`, which the container does not ship.

Four tests are `#[ignore]`d and none is a skipped assertion. Two are the ring's
concurrency probes, which need release codegen and seconds of wall clock:

```sh
cargo test --release --test ring_race_test -- --ignored --nocapture
```

**That is a real gap, not a tidy arrangement.** They guard the memory-ordering fences
in `src/ring.rs` — the most serious defect found in this codebase — at a measured
detection rate of 10-20% per run, and removing the fences leaves the default suite
green on both platforms. `tests/ring_race_test.rs` says what would fix it.

`src/main.rs` is **not the deck**; it is a bring-up CLI:

```sh
cargo run -- <file>...              # what the file layer makes of each path
cargo run -- --drain <file>         # pull every frame through window, ring, callback
cargo run -- --device=hw:0,0 <file> # play for real (Linux; hw: only, never plughw)
cargo run -- --rt-check[=CPU]       # apply the realtime setup, read back what took
cargo run -- --media-check[=PATH]   # the medium's state, and its root folder
```

The default prints `PLAYS` with container, rate, depth and window, or `REFUSED` with
which of the four reasons applies. Test fixtures come from the same hand-written
writers the null test uses, rather than from `sox` — so they do not trust another
implementation of the thing under test:

```sh
DECK_PI_DEMO_DIR=/tmp/deck-demo cargo test --test emit_demo -- --ignored
```

Two measurements against real material are `--ignored`, because one needs the stick
and the other needs the Pi:

```sh
cargo test --release --test render_bench -- --ignored --nocapture
DECK_PI_MUSIC_DIR=/media/stick/Music cargo test --release \
    --test font_covers_library -- --ignored --nocapture
```

The first prints what a frame costs to draw on the machine it runs on; the second
asks whether the panel font contains every character in every name on the stick, and
names the files it does not. On 2026-09-16 it found **87 of 2192 names** with a
character neither face has; all but **5** fold to ASCII that reads as the original
(`é` as `e`, `…` as `...`), and the five that do not are three kanji outside
`japanese3`, plus a Cyrillic `С` sitting inside an otherwise Latin name.

`tools/panel-compare` is a standalone crate that renders a folder listing at all six
candidate panel geometries, at true physical size at 300 dpi with a 10 mm rule. Print
at 100 % and measure the rule before trusting any millimetre figure.

## Later

A pitch fader and a jog wheel. Both reduce to one variable — read at rate `r` — so
they arrive as one mechanism, resampled through libsoxr in its variable-rate mode.
Bit-perfect playback then stops being automatic and becomes an explicit **button**,
because a resampler cannot be made transparent by handing it a ratio of 1.0. That
button is also the fallback for rates the resampler cannot sustain: a hi-res track
plays bit-perfect without pitch rather than not playing.

## Docs

| | |
|---|---|
| [hardware.md](docs/hardware.md) | Boards, jumpers, power, the 40-pin header map, assembly, bring-up |
| [controls.md](docs/controls.md) | What pressing each control *does* |
| [architecture.md](docs/architecture.md) | Format scope, playback model, program shape, v2 |
| [implementation.md](docs/implementation.md) | ALSA specifics, FFI, realtime setup, `config.txt` |
| [decisions.md](docs/decisions.md) | Why each choice went as it did, and which were **reversed** |
| [firmware/src/main.rs](firmware/src/main.rs) | The Pico's side, documented where it is written: the pinout, and which HID usage each control sends |

Open questions are [GitHub issues](https://github.com/tamatebox/deck-pi/issues).
Read `decisions.md` before revisiting a design choice: several were reversed and the
superseded reasoning is plausible enough to re-derive by accident.

**The labels in those documents are load-bearing.** A figure says whether it is
*measured* or *estimated*; a premise says whether it was *supplied* or *assumed*; a
hardware fact says whether it was confirmed against the boards, the vendor's
document, or kernel source. An unlabelled estimate is indistinguishable from a fact
three turns later, which is how several wrong premises survived as long as they did.
Two that matter for anyone quoting this: **nothing has been checked against the
physical boards**, and **the A53 resampler budget is an estimate** that has not been
run.

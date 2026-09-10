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
Pi 3B+  --I2S-->  IsolatorPi III  --isolated I2S-->  Digi2 Pro  --S/PDIF-->  DAC
```

One Pi is one deck, which is what makes it safe to reopen the audio device for every
track — and that is what lets each track play at its own sample rate. Operation is
entirely network-independent: the stick is prepared elsewhere and carried, and the
Ethernet cable can be out during a set.

## Status

**Nothing has run on hardware.** The Pi and the boards are not assembled, so the
ALSA sink has never opened a real device. Do not read "audio callback" or "ALSA sink"
as "sound comes out".

The v1 read path is built and tested — FFI, format vetting, the locked ring, the
window thread, the transport, the callback, the ALSA sink, the realtime setup, the
browser, media watch, the cue store and input — with bit-perfection verified end to
end at both depths and all six rates, across four of the five containers in scope
(**Wave64 is accepted and untested**).

The app loop is partly built: the period loop, the track lifecycle, the dispatch and
the control loop that reads `/dev/input`. Still missing are **media watch wired to
the deck** and **the display, which has no file at all**. `src/main.rs` is a bring-up
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
| IsolatorPi III | 5 kV galvanic isolation, master-mode capable |
| Clean 5 V supply | under 200 mA, feeds the isolated side via J1 |

The Digi2 Pro carries separate oscillators for the 44.1 and 48 kHz families and runs
as clock master, so both come out of an exact crystal rather than the Pi's fractional
PLL. The isolator keeps the Pi's ground noise off the audio boards and gives the
controls a non-isolated header of their own. The 3B+ soft-throttles to 1.2 GHz at
60 °C by design, and a deck runs a continuous load inside a box, so the headline
clock is a sprint clock.

Three assembly steps are easy to get wrong and produce **no error when wrong**: the
master/slave jumpers, the clean-side power feed, and leaving GPIO 5/6 free —
[docs/hardware.md](docs/hardware.md).

## Controls

A detented rotary encoder for browsing, plus BACK, PLAY/PAUSE, CUE/STOP, and FF and
REW — hold to seek, tap to change track. ENTER is the encoder's push. The display
shows the browser, the transport, and the rate and depth actually in use, which is
how you confirm the chain is doing what it claims.

**Seven switches, two encoders and a fader, and the header is then full**, the v2
unity button being the seventh. The panel is an SPI colour TFT. Seeking is silent in
v1: an audible scan needs the resampler, which would give v1 a second mode. Encoders
and buttons are decoded in the kernel through device-tree overlays, never polled from
userspace.

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

Rust, one process, one binary. libsndfile is a system library, found through
`pkg-config`:

```sh
brew install libsndfile pkg-config          # macOS
sudo apt install libsndfile1-dev pkg-config # Debian / Raspberry Pi OS
cargo test
```

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

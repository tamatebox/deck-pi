# deck-pi

A bit-perfect single-deck DJ transport built on a Raspberry Pi 3B+. It reads a USB
stick, browses it by folder, and plays WAV/AIFF out as S/PDIF with the source
samples reaching the DAC untouched.

**Status.** The v1 read path is built and tested — libsndfile FFI, format vetting,
the locked int32 ring, the window thread, the transport, the audio callback, the
ALSA sink, the realtime process setup, the browser, media watch, the cue store and
input — with bit-perfection verified end to end at both depths and all six rates,
across four of the five containers in scope. **Wave64 is accepted and untested**;
the other four are WAV, AIFF, AIFF-C `sowt` and RF64. That is the software half
only. **Not started: the display, and the app loop that would join these modules
together.**

**Nothing has run on hardware.** The Pi and the boards are not assembled, so the
ALSA sink has never opened a real device and the `hw_params` half of the null test
is unproven. Do not read "audio callback" or "ALSA sink" as "sound comes out".

```
USB stick or drive — exFAT or HFS+, mounted read-only
   |  folder tree is the index; no import step, no database
libsndfile, in the window thread     <- byte swap, 24-bit unpack, no deadline
   |
locked int32 ring                    <- the audio callback reads only this
   |  ALSA hw:, output rate matched to the file
Pi 3B+  --I2S-->  IsolatorPi III  --isolated I2S-->  Digi2 Pro  --S/PDIF-->  DAC
```

One Pi is one deck. A second deck is a second Pi — which is what makes it safe to
reopen the audio device for every track, and that in turn is what lets each track
play at its own sample rate.

Operation is entirely network-independent. The medium is prepared elsewhere and
carried; Ethernet exists for maintenance, and the cable can be out during a set.

**exFAT and HFS+** are both supported, being the two filesystems macOS writes
natively that also have mature in-tree Linux drivers — so an existing Mac OS
Extended (Journaled) drive full of music works as it is, with no reformatting. The
driver forces a journaled volume read-only, which is the policy here regardless.

## The idea

**The boundary that matters is the deadline, not the machine.** The audio callback
reads locked RAM and nothing else. Everything that touches a file, a header, a
byte order or a sample width happens on a thread that is allowed to block.

Draw that line correctly and the work is cheap wherever it runs. Uncompressed PCM
has nothing expensive in it: a byte swap is one ARM instruction, unpacking 24-bit
to 32-bit is a few more, and neither is a decoder. What was never affordable was
doing them *under a deadline*.

So there is no import step and no library database. Files play as they are, and
the deck is a transport rather than a media server.

No decoding, no sample-rate conversion, no gain stage. The output rate is set from
the file when it loads, so at unity there is nothing to convert: the bytes on the
stick are the bytes on the wire. That is a property a test can assert, not a
claim — and it is asserted from both ends, in software and at `hw_params`.

## What plays

Exactly what the DDC can send, so there is no second rule to remember.

| | |
|---|---|
| Container | WAV, AIFF, AIFF-C, RF64, Wave64 |
| Sample rate | 44.1 / 88.2 / 176.4 and 48 / 96 / 192 kHz |
| Bit depth | int16 and int24 only |

Every supported conversion is a pure shift, which is what makes v1's
bit-perfection unconditional rather than conditional. 32-bit float would need a
clipping or scaling decision, and scaling is a gain stage — so it is out of scope
rather than handled. Anything refused is converted when preparing the stick, where
there is no deadline and the lossy choices can be made deliberately.

Every rejection is decidable from the header, so the browser refuses on highlight
rather than on PLAY. Nothing surprises you mid-set.

## Hardware

| | |
|---|---|
| Raspberry Pi 3B+ | 1 GB, Cortex-A53, 1.4 GHz — sized against 1.2 GHz, see below |
| HiFiBerry Digi2 Pro | WM8804, dual-domain clock, no volume control by design |
| IsolatorPi III | 5 kV galvanic isolation, master-mode capable |
| Clean 5 V supply | under 200 mA, feeds the isolated side via J1 |

The Digi2 Pro carries separate oscillators for the 44.1 and 48 kHz families and
runs as clock master, so both families come out of an exact crystal rather than
the Pi's fractional PLL. The isolator keeps the Pi's ground noise off the audio
boards, and gives the buttons and display a non-isolated header of their own — so
the control surface sits on the far side of the gap from the audio.

The 3B+ is specified at 1.4 GHz but soft-throttles to 1.2 GHz at 60 °C by design,
so 1.2 GHz is the sustained figure everything is sized against. A deck runs a
continuous load inside a box; the headline clock is a sprint clock.

Three assembly steps are easy to get wrong and produce **no error when wrong**:
the master/slave jumpers, the clean-side power feed, and leaving GPIO 5/6 free.
See [docs/hardware.md](docs/hardware.md).

## Controls

A detented rotary encoder for browsing, plus BACK, PLAY/PAUSE, CUE/STOP, and FF
and REW — hold to seek, tap to change track. ENTER is the encoder's push. The
display shows the browser, the transport, and the sample rate and bit depth
actually in use, which is how you confirm the whole chain is doing what it claims.
Which panel is still open — one candidate is a TFT, not an OLED.

Seeking is silent in v1. An audible scan needs the resampler, which would give v1
a second mode and end the unconditional bit-perfection; in v2 it becomes one more
value of the rate variable.

Encoders and buttons are decoded in the kernel through device-tree overlays, not
polled from userspace.

## Track length

Bounded by the **source container**, not by RAM. The window thread keeps only a
region around the playhead resident, so a three-hour file costs the same memory
as a three-minute one — and because the ring is int32 whatever the source was,
the cost is constant in sample rate too.

At the 2 GiB compatible ceiling:

| | int16 | int24 |
|---|---|---|
| 44.1 kHz | 3 h 23 m | 2 h 15 m |
| 48 kHz | 3 h 06 m | 2 h 04 m |
| 96 kHz | 1 h 33 m | 1 h 02 m |
| 192 kHz | 47 m | 31 m |

RIFF and AIFF chunk sizes are 32-bit and widely treated as signed, so 2 GiB is
the compatible ceiling. Longer needs RF64 or Wave64, which libsndfile reads at no
extra cost — so long-form work at high rates is a container choice, not a limit.

## Software

Rust, one process, one binary: engine, browser and display. The callback rules are
machine-enforced rather than aspirational — `assert_no_alloc` fails loudly on an
allocation inside the callback, which has no equivalent in C. It wraps Rust's
`GlobalAlloc`, though, so it cannot see a C library calling glibc directly: that
costs nothing for libsndfile, which runs where allocation is allowed anyway, and it
is exactly the blind spot v2's resampler sits in. Checking that needs an
`LD_PRELOAD` interposer, and `implementation.md` carries the one configuration
requirement that came out of doing so.

Files are read through a hand-written libsndfile FFI, about forty lines of
`extern "C"`. `sf_readf_int`'s documented convention already does the byte swap
and the 24-bit unpack, so neither is our code; the ring then holds the result
shifted right 8 to match `S24_LE`, the only useful output format the drivers
offer. Every step is a shift, so nothing costs a bit.

## Building and running it

libsndfile is a system library, found through `pkg-config`:

```sh
brew install libsndfile pkg-config          # macOS
sudo apt install libsndfile1-dev pkg-config # Debian / Raspberry Pi OS
cargo test    # 183 tests on Linux, 170 on macOS; green in debug and release
```

Four tests are `#[ignore]`d and none is a skipped assertion. One is the demo-file
generator below. One is the *subject* of a negative control — the test that proves
the no-allocation enforcement actually aborts spawns it deliberately, so running it
directly would abort the harness. The other two are the ring's concurrency probes:

```sh
cargo test --release --test ring_race_test -- --ignored --nocapture
```

They need seconds of wall clock and release codegen, which is why they are not in
the default run — **and that is a real gap, not a tidy arrangement.** They guard
the memory-ordering fences in `src/ring.rs`, the most serious defect found in this
codebase, and their measured detection rate is 10-20% per run. Removing the fences
leaves the default suite green on both platforms. `tests/ring_race_test.rs` carries
the numbers and says what would actually fix it.

**Most of it builds and is tested off the target.** The ALSA sink is Linux-only and
sits behind an `AudioSink` trait, so on a Mac the same engine drives a capture sink
instead — the file layer, ring, transport, callback and the null test all run there.
What cannot run off the Pi is the half that needs the hardware.

`src/main.rs` is **not the deck.** It is a bring-up CLI:

```sh
cargo run -- <file>...              # what the file layer makes of each path
cargo run -- --drain <file>         # pull every frame through window, ring, callback
cargo run -- --device=hw:0,0 <file> # play for real (Linux; hw: only, never plughw)
cargo run -- --rt-check[=CPU]       # apply the realtime setup, read back what took
cargo run -- --media-check[=PATH]   # the medium's state, and its root folder
```

The default prints one line per path — `PLAYS` with the container, rate, depth and
window, or `REFUSED` with which of the four reasons applies. `--drain` adds frames,
waits, underruns and peak. `--device=` additionally checks that the card exposes no
mixer control and that `/proc/asound` reports back the rate and format asked for.

`--rt-check` is separate because it changes the process: `mlockall` is process-wide
and `SCHED_FIFO` would put the tool's own bookkeeping at realtime priority. It
prints the limits, applies the setup, and reads back the policy, priority, locked
memory and affinity — so an unprivileged run says which `/etc/security/limits.conf`
line is missing rather than that something was refused.

Test files come from the same hand-written writers the null test uses, rather than
from `sox` or `ffmpeg`, so the fixtures are not trusting another implementation of
the thing under test:

```sh
DECK_PI_DEMO_DIR=/tmp/deck-demo cargo test --test emit_demo -- --ignored
```

That writes six playable files across the containers, depths and rates in scope,
two that must be refused, and one that is not audio at all.

### tools/panel-compare

A standalone crate, not part of the build. It renders the same folder listing with
real Japanese filenames at all six candidate panel geometries, at true physical
size at 300 dpi with a 10 mm rule, and asserts that every derived glyph size
reproduces the figures in `decisions.md`:

```sh
cd tools/panel-compare && cargo run   # writes out/, which is not committed
```

It exists because open question 1 was being argued from arithmetic. Print the sheet
at 100 % and measure the rule before trusting any millimetre figure — a frame on a
monitor is at whatever scale the monitor makes it.

## Later

A pitch fader and a jog wheel. Both reduce to one variable — read at rate `r` —
so they arrive as one mechanism, resampled through libsoxr in its variable-rate
mode. Bit-perfect playback then stops being automatic and becomes an explicit
button, because a resampler cannot be made transparent just by handing it a ratio
of 1.0.

That button is also the fallback for rates the resampler cannot sustain: a hi-res
track plays bit-perfect at unity, without pitch, rather than not playing. So the
open benchmark decides which rates get pitch and jog, not whether the board is
viable.

The v1 read path, control thread and rate variable accept this without rework. One
thing did not, and it was found by writing the test rather than by reading the
claim: the window's *filling policy* appended forward only, so a **descending**
playhead — a reverse jog, or a held REW — was served 0 of 12 periods. The window's
two halves are now defined relative to the direction of travel rather than to
increasing frame number, which serves 12 of 12. One cost remains and is stated
rather than hidden: reverse playback misses one period per relocation, because an
append-only ring reads a descending playhead's next input *last*. Reverse playback
is served, not gapless. See [docs/architecture.md](docs/architecture.md).

## Docs

- [docs/hardware.md](docs/hardware.md) — board stack, jumpers, GPIO map, assembly checklist
- [docs/architecture.md](docs/architecture.md) — format scope, playback model, program shape, v2 design
- [docs/implementation.md](docs/implementation.md) — ALSA specifics, FFI, realtime setup, dependencies
- [docs/decisions.md](docs/decisions.md) — decision log, reversed advice, open questions

Several design decisions were reversed while working this out, and the superseded
reasoning is plausible enough to re-derive by accident. `decisions.md` records why
each was dropped; read it before revisiting a choice.

**Open questions are [GitHub issues](https://github.com/tamatebox/deck-pi/issues),
and the reasoning behind them stays in `decisions.md`** — deliberately not both, so
there is only one copy of an analysis to keep current. An issue says what would
close it; the doc says why it is hard.

**The labels in those documents are load-bearing, so read them.** A figure says
whether it is *measured* or *estimated*, a premise says whether it was *supplied* or
*assumed*, and a hardware fact says whether it was confirmed against the boards, the
vendor's own document, or kernel source. The distinction is not politeness: an
unlabelled estimate is indistinguishable from a fact three turns later, which is how
several wrong premises survived as long as they did — each recorded in `decisions.md`
with what it changed on the way out.

Two consequences for anyone quoting this work:

- **Nothing here has been checked against the physical boards**, which are not
  assembled. `hardware.md` marks what still needs them. The jumper settings matter
  most, because getting master mode wrong still produces audio — through the
  high-jitter PLL path the whole build exists to avoid.
- **The A53 resampler budget is an estimate** and is labelled as one throughout. It
  decides which rates get pitch in v2, and it has not been run.

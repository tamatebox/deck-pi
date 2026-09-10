# CLAUDE.md

## Project

Bit-perfect single-deck DJ transport on a Raspberry Pi 3B+. It plays WAV/AIFF
**straight off removable USB media** — exFAT or HFS+, read-only, no import step,
no database, browsed by folder tree — out to a HiFiBerry Digi2 Pro over I2S, galvanically isolated by an
IsolatorPi III, and out as S/PDIF to an external DAC. One Pi is one deck; a second
deck is a second Pi. Operation is entirely network-independent; Ethernet is for
maintenance only.

The hardware and the software design are settled — read the docs before proposing
changes to either.

**Status.** The v1 read path is built and tested: the libsndfile FFI, format
vetting, the locked int32 ring, the window thread, the transport, the audio
callback, the ALSA sink behind an `AudioSink` trait, the realtime process setup
and the browser, media watch, the cue store and input. The suite is green in
debug and release on Linux/aarch64 and on macOS, where the Linux-only paths
compile out, and clippy is clean on both — **and that was never evidence about
the things this project gets wrong, which is recorded here so the line is not
read as though it were.** Of the sixteen faults found in the review that
produced most of these tests, clippy caught **zero**. That is not a failure of
the tool: memory ordering, a mechanism with no caller, a comment that agrees
with the code and is wrong anyway, an unstated premise about the hardware —
every one of them is invisible to a lint by construction, because they are about
what the code *means* against what a document, a device or a peer does. A linter
that finds `map_identity` is doing its job. The mistake would be counting it as
verification. Read the line as "no default-group lint fires" and nothing else;
`docs/implementation.md`'s *What reads as handled and is not* is where the
checks that would have found those sixteen are written down.

**Some tests are `#[ignore]`d and green does not include them**: the ring-race
probes and the demos. The ring pair is the regression guard for the most serious
defect found so far and it is *opt-in* — see `tests/ring_race_test.rs`, which
records its own measured detection rate of 10-20% per run.

Bit-perfection is verified end to end at both depths and all
six rates for **four** of the five containers in scope — WAV, AIFF, AIFF-C `sowt`
and RF64. **Wave64 is accepted and never tested**: `tests/fixtures` writes its
files by hand on purpose, so covering it means writing a Wave64 encoder rather
than adding a line, and the sample path after the header is byte-identical to
WAV's. Said plainly because "every container" was the claim here and it was one
short. And all of this is **only the software half.**

**Not started, no file at all:** the display, and the app loop that would join
these modules to each other — `src/main.rs` is still a bring-up CLI.
**Nothing has run on hardware** — the Pi and the boards are not assembled — so the
`hw_params` half of the null test is unproven and the ALSA sink has never opened a
real device. Do not read "audio callback" or "ALSA sink" as "sound comes out".

`src/main.rs` is not the deck. It is a bring-up CLI: it reports what the file layer
makes of a path, and `--drain` runs a file through the window thread, ring and
callback and prints frames, waits and peak.

- @docs/hardware.md — board stack, jumpers, GPIO map, assembly checklist
- @docs/architecture.md — format scope, playback model, program shape, threading, v2 design
- @docs/implementation.md — ALSA specifics, libsndfile FFI, realtime setup, dependencies
- @docs/decisions.md — decision log, reversed advice, open questions

`architecture.md` is the design and should be stable. `implementation.md` is what
to type and what fails silently, and it churns as crates and APIs move — do not
promote things from it into the design doc.

## Operating principles

- Read `docs/decisions.md` before revisiting a design choice. Several decisions
  were reversed during design and the superseded reasoning is still plausible
  enough to be re-derived by accident; the log says why each was dropped.
- Hardware facts — jumper positions, pin assignments, power feed — are
  load-bearing and silent when wrong. Never guess one. Cite `docs/hardware.md`
  or ask.
- Before deferring something to "when the hardware arrives", check whether it is
  actually a *driver* question. Formats, rates and pin roles are declared
  statically in kernel source and can be settled now; the boards' own jumpers and
  connectors cannot. Two open questions were closed this way — see
  `docs/implementation.md`.
- **Usage facts are the user's to supply, not yours to infer.** What material
  exists and in what formats, what the medium is, where the deck gets used, how it
  gets played — none of it is derivable from the code, the boards or the datasheets.
  Five such premises were invented during design and every one had to be unwound,
  each only after it had already become the foundation of later conclusions. If one
  is missing and needed, ask. If you must proceed without it, say in the text that
  it is an assumption, so it can be found and pulled back out.
- **Another session may be in this tree.** Mid-edit states of tracked files are
  readable by peers and get acted on: a section proposing a configured
  output-rate ceiling was read and implemented while it briefly existed, then
  reverted once the finished text said the idea had been dropped twice. Land doc
  changes in coherent steps rather than leaving speculative sections sitting in
  the tree, and check `ListAgents` before assuming you are alone in it.
- Say whether a number is measured or estimated. The A53 resampler budget in
  `docs/architecture.md` is an estimate and is labelled as one; do not launder it
  into a fact. The same goes for a premise: an unlabelled one is indistinguishable
  from a settled fact three turns later, which is exactly how the five above
  survived as long as they did.
- **No number in this file may change when you commit.** Hardware figures, rates,
  pin numbers, the arithmetic behind a design choice — all fine, they are
  properties of the thing. A count of passing tests is not: it is a measurement of
  the tree at one instant, in the file every session loads at start and nobody
  re-reads. It went stale within six commits, twice in one day, and a stale
  measurement is worse than none because it still reads as measured. Say the suite
  is green; the exact figure belongs next to the command that prints it, in
  `README.md`, where the reader sees the real one seconds later.

  The same test applies to anything else here: if landing an ordinary commit could
  falsify the sentence, it is status and does not belong. `decisions.md` had this
  problem first — a status line at the top of the reasoning log, three months
  stale — and moving it here fixed the wrong half.
- The 3B+ is specified at 1.4 GHz but **sizing is against 1.2 GHz**, because its
  soft temperature limit drops the clock there at 60 C. Do not "correct" the
  1.2 GHz figures upward — see `docs/hardware.md`.

## Invariants

- **No gain stage in the playback path, ever.** Bit-perfect output is the point
  of the project. The Digi2 Pro deliberately exposes no volume control; software
  must not add one, not even a "temporary" one for testing.
- **Output sample rate follows the source file, per track.** Never resample to a
  fixed output rate. At unity pitch the source samples must reach the DAC
  untouched, and that is only possible when the rates already match.
- **The audio callback allocates nothing, locks nothing, does no I/O, and cannot
  fault.** It reads a locked int32 ring and nothing else. This holds from the first
  commit, while the load is still trivial enough to get away with breaking it. The
  no-fault half is what makes a stick pulled mid-set safe rather than a SIGBUS
  inside the audio thread.
- **Sources are int16 or int24 only, 44.1 to 192 kHz.** That is exactly what the
  Digi2 Pro can send, so there is no second rule. It is also what makes v1's
  bit-perfection unconditional: every supported conversion is a pure shift.
  32-bit float would need a clipping or scaling decision, and scaling is a gain
  stage — so it is out of scope rather than handled.
- **GPIO 5 and 6 are reserved** for oscillator select. They are Pi pins routed
  *through* the isolator to the audio card (J6 pins 29/31, "isolated GPIO5 and
  GPIO6"), so removing the isolator does not free them. They choose between the
  44.1 and 48 kHz crystals, which is the ability to play either family exactly.
  Never assign them to buttons, encoders or a display.
- **I2S is GPIO 18, 19, 20 *and* 21 — four pins.** GPIO 20 is PCM_DIN, unused for
  playback but claimed by the interface anyway, and HiFiBerry's GPIO page says so
  outright. A button was once assigned to it here because a table listed only three.
  Take the reserved set from that page, not from what looks unused.
- **The playback position accumulator is float64.** float32 has a 24-bit mantissa,
  so past 2^23 samples (~190 s at 44.1 kHz) the fractional part is gone and
  interpolation silently stops working.

## Language

**Rust, everything, one process.** Engine, browser and display. Python owns nothing
on the deck — it never could own the realtime path (GC pauses at these buffer
sizes drop out), and the reasons for keeping it on the UI side dissolved: there is
no database, no library management, and the browser needs libsndfile too, so a
language boundary would mean binding it twice.

**There is no Python in this project at all.** Preparing a stick is not a component
of it: converting a file the deck refuses is done with `sox` or `ffmpeg`, on some
other machine, and the deck cannot tell how a file was made. The project's only
obligation there is that the UI says *why* a file will not play.

The callback rules are machine-enforced, not aspirational: `assert_no_alloc` fails
loudly on allocation inside the callback. Keep it that way from the first commit.

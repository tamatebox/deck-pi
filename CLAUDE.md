# CLAUDE.md

Bit-perfect single-deck DJ transport on a Raspberry Pi 3B+. Plays WAV/AIFF straight
off removable USB media, read-only, no import step, no database, out over I2S to a
HiFiBerry Digi2 Pro through an IsolatorPi III, S/PDIF to an external DAC. One Pi is
one deck. Network-independent; Ethernet is maintenance only.

**Rust, one process** — engine, browser and display. **There is no Python in this
project at all.** Preparing a stick is done elsewhere with `sox` or `ffmpeg`; the
deck's only obligation is to say *why* a file will not play.

## Invariants

Short, load-bearing, and silent when broken — which is why they are here rather
than in a document you would have to think to open.

- **No gain stage in the playback path, ever.** Not even temporarily, for testing.
- **Output sample rate follows the source, per track.** Never resample to a fixed
  rate. Unity pitch means the rates already match.
- **The audio callback allocates nothing, locks nothing, does no I/O, and cannot
  fault.** It reads a locked int32 ring and nothing else. `assert_no_alloc`
  enforces the first clause; nothing enforces the rest.
- **Sources are int16 or int24 only, 44.1 to 192 kHz.** Exactly what the Digi2 Pro
  can send, which is what makes every conversion a pure shift.
- **GPIO 5 and 6 are reserved** for oscillator select, routed *through* the
  isolator. Removing the isolator does not free them.
- **I2S is GPIO 18, 19, 20 *and* 21 — four pins**, physical 12, 35, 38, 40. GPIO 20
  is PCM_DIN, unused for playback and claimed anyway.
- **The playback position accumulator is float64.** float32 loses the fraction past
  ~190 s at 44.1 kHz.
- **Size against 1.2 GHz, not 1.4.** The 3B+ soft-throttles at 60 C.

## How to work here

- **A fact from a document this project cites is read, not recalled.** Someone
  guessing knows they are guessing; someone recalling believes they know, so the
  sentence comes out confident *and citation-shaped* — and is believed harder for
  it. This has produced real errors, twice. Open the page.
- **Usage facts are the user's to supply, not yours to infer.** What material
  exists, what the medium is, how the deck gets played. Seven invented premises
  had to be unwound. If one is missing, ask; if you must proceed, label it.
- **Say whether a number is measured or estimated.** An unlabelled estimate is
  indistinguishable from a fact three turns later.
- **Read `docs/decisions.md` before revisiting a design choice.** Several were
  reversed and the superseded reasoning is plausible enough to re-derive by
  accident.
- **No status in this file.** If landing an ordinary commit could falsify a
  sentence here, it belongs in `README.md` next to the command that prints it.
- **Another session may be in this tree.** Check `ListAgents`; land doc changes in
  coherent steps rather than leaving speculative text sitting in the tree.

## Where things are — read on demand, not up front

None of these are auto-loaded. Open the one the task touches; that is deliberate,
and `docs/README.md` says why.

| | |
|---|---|
| `docs/hardware.md` | Boards, jumpers, power, the 40-pin header map, how many of what, assembly and bring-up. **Any pin, jumper, connector or part-count question.** |
| `docs/controls.md` | What pressing each control *does*, transcribed from the CDJ-350 manual and marked where the deck departs. **Any question about what a button means.** |
| `docs/architecture.md` | Format scope, the ring and window, transport, threading, v2. **The design; should be stable.** |
| `docs/implementation.md` | ALSA calls, libsndfile FFI, realtime setup, mount, `config.txt` and the overlays, dependencies — and *What reads as handled and is not*, the checklist for this project's recurring defect. **What to type, and what fails silently.** |
| `docs/decisions.md` | Why each settled decision went the way it did, and the log of the ones that were **reversed** — whose superseded reasoning has no other home. **Before changing a design choice.** |
| `README.md` | Status, what is built, how to run the suite. |
| [issues](https://github.com/tamatebox/deck-pi/issues) | Every open question, and the analysis behind it. |

# Architecture

The design. `decisions.md` has why each choice went the way it did; this file has
the shape that resulted.

## The one idea

**The boundary that matters is the deadline, not the machine.** The audio callback
reads locked RAM and nothing else; everything that touches a file, a header, a byte
order or a sample width runs on a thread allowed to block.

Uncompressed PCM has nothing expensive in it — a byte swap is one ARM instruction,
unpacking 24-bit to 32-bit is a few more, and neither is a decoder. What was never
affordable was doing them *under a deadline*. That is what makes bit-perfect
playback and a sustained 1.2 GHz A53 compatible.

## Library

**The USB stick is the library** — prepared elsewhere, mounted read-only, never
written, the folder tree as the index. No import step, no database. The Pi's SD card
holds the OS and the state the deck creates: cues, keyed by volume UUID plus
relative path. The stick is content; the Pi owns what it made.

### What plays

| | |
|---|---|
| Container | WAV, AIFF, AIFF-C, **RF64, Wave64** |
| Sample rate | 44.1 / 88.2 / 176.4 and 48 / 96 / 192 kHz |
| Bit depth | **int16 and int24 only** |
| Combinations | all **6 x 2 = 12**, no preferred subset |
| Channels | stereo (mono duplicated to both — lossless) |

If the Digi2 Pro can send it, the deck plays it. There is no second rule.

### What does not

| Rejected | Reason |
|---|---|
| 32-bit int or float | Above the 24-bit ceiling; float also needs a clipping or scaling decision, and scaling would be a gain stage |
| 8-bit | Below int16, and unsigned by convention |
| 32 kHz, 22.05 kHz | Outside the interface limit |
| MP3, FLAC, AAC | A decoder would run on the Pi |
| DSD | No DoP board is fitted, deliberately |

**Every one is decidable from the header alone**, so the browser refuses on
*highlight* rather than on PLAY, and nothing here can surprise you mid-set.

Anything wanted is converted when preparing the stick — `sox` or `ffmpeg`, both
buildable against libsoxr, which is the same library v2 uses on the deck. Off the
deck there is no deadline and the lossy choices are deliberate.

### Track length ceiling

Not RAM, which is constant in track length. The limit is the **container**: RIFF and
AIFF chunk sizes are 32-bit, and many implementations treat the field as signed, so
2 GiB is the compatible ceiling.

| at 2 GiB | int16 | int24 |
|---|---|---|
| 44.1 kHz | 3 h 23 m | 2 h 15 m |
| 48 kHz | 3 h 06 m | 2 h 04 m |
| 88.2 kHz | 1 h 41 m | 1 h 08 m |
| 96 kHz | 1 h 33 m | 1 h 02 m |
| 176.4 kHz | 51 m | 34 m |
| 192 kHz | 47 m | 31 m |

**RF64 and Wave64 lift it entirely.** The risk sits with whatever *wrote* the file:
a tool running past 2 GiB into a plain WAV can emit a wrapped size field, and the
file then plays and stops early with nothing to indicate why — so the browser should
treat an implausible declared length as suspect.

Neither capacity nor bus contention constrains anything: 256 GB holds 62 hours at
the very worst (192/24), the audio path is I2S so the stick has the single USB 2.0
bus effectively to itself, and sustained read peaks at 1.15 MB/s.

## Playback

**The window thread reads through libsndfile into a locked int32 ring around the
playhead. The audio callback reads the ring and nothing else.**

Samples land in the ring in the output's own `S24_LE` layout — `sf_readf_int`'s
left-justified value shifted right 8 — so the callback has one format, one path, no
branch on source depth and nothing to convert. Every step is a pure shift, so the
chain is lossless. **The shift belongs in the window thread**: it could legally go in
either, but moving work off the deadline is the whole organising idea.

**The window is sized in bytes**, `min(60 s, N MiB)`. Time alone would swing RAM 4x
across the supported rates; a byte cap holds it flat and degrades window length
instead. There is no bit-depth axis, the ring being int32 whatever the source was.
So **RAM cost is constant in both track length and sample rate** — the first
decouples length from the Pi's 1 GB, the second lets an unexpected hi-res file play
with a shorter window instead of failing. N is not chosen
([#9](https://github.com/tamatebox/deck-pi/issues/9)); at 64 MiB the window runs
±60 s at 48 kHz down to ±21 s at 192 kHz.

**The two window halves are relative to the direction of travel**, not to increasing
frame number. An append-only ring accumulates for free only on the side the playhead
has passed, and a forward-only refill gave a *descending* playhead a window laid out
entirely ahead of it: 12 relocations, **0 of 12 periods served**, measured. Direction
comes from successive playhead values, so nothing new is plumbed, and an explicit
seek clears it. Measured after: 12 of 12. One cost stays — relocating discards the
window and a descending playhead's next input arrives last, so the callback asks for
it once per relocation before it is there. Reverse playback is *served*, not gapless.

Pulling the stick behaves like a CDJ, for the same reason: what is already resident
keeps playing. The grace period is the forward half of the window — bounded rather
than guaranteed — and the window thread is where the failure surfaces, having no
deadline.

**Position is float64.** A float32 accumulator's 24-bit mantissa reaches spacing 1.0
past 2^23 samples, ~190 s at 44.1 kHz, after which the fraction is gone and
interpolation quietly stops working — audible as the back half of a track degrading,
with nothing pointing at the cause.

**Output rate follows the source**, set when the track loads. All six rates belong to
the 44.1 or 48 kHz family, so crossing families **switches oscillators** via the
GPIO 5/6 lines `hardware.md` reserves — the driver does that from `hw_params` and the
application only ever sets a rate. Staying within a family costs nothing: the machine
driver returns early when the rate is unchanged.

**v1 is unconditionally bit-perfect.** No pitch and no jog means no second mode, and
every supported conversion is a pure shift. FF and REW do not break it either: they
are a **silent seek**, position advancing with no audio produced, so there is never a
moment when something other than the source reaches the DAC.

Four ALSA requirements, because it converts silently when asked wrongly: **`hw:`
only** (`default` mixes, `plughw` resamples), **the exact rate, never the nearest**
(the convenience call succeeds at a different rate and reports nothing), **no
software volume**, and note that **a card with a volume control scales even on
`hw:`** — which the Digi2 Pro avoids by having none at all. The calls and flags are
in `implementation.md`.

**What the DAC accepts is out of scope.** S/PDIF is unidirectional, so a DAC that
cannot lock goes silent and the software can never detect it. The display shows the
rate in use, which is as much diagnosis as exists.

## v2: pitch and jog

**One rate variable.** Pitch is `r` near 1.0, jog is `r` varying per block and going
negative, pause is `r = 0`. One resampling read path carries all three.

**libsoxr, not libsamplerate.** The latter's three sinc converters are all 97 dB
SNR — roughly 16 bits — which would cap the whole system at 16-bit quality whenever
pitch is off centre. libsoxr specifies quality in bits, has **`SOXR_VR`** as a
first-class variable-rate mode, and slews rate changes through
`soxr_set_io_ratio`'s third argument, which is the fix for zipper noise at block
boundaries. `passband_end`, `stopband_begin` and `phase_response` are directly
settable, so bandwidth trades against CPU continuously.

**`soxr_create` must be given a ratio range that brackets the one the deck uses.**
Declare 1:1 and set ratios outside it and `soxr_process` reallocates on the audio
thread, unboundedly, while the audio stays correct and `assert_no_alloc` stays
silent — it cannot see libsoxr at all. The range is ±10% and known in advance, so
this is a line of setup code, and a load-bearing one; `implementation.md` carries it
with the measurements.

**The CPU budget is an estimate, not a measurement.** Benchmark precision x output
rate x pitch range on the actual Pi, **thermally soaked**
([#3](https://github.com/tamatebox/deck-pi/issues/3)) — a cold run measures a clock
the board will not hold for a set. Downsampling costs extra: a 192 kHz source at
96 kHz output is about twice a 96 kHz source at the same output.

**Unity is a button, not a deadband on the fader.** Bit-perfect cannot be recovered
by feeding the resampler a ratio of 1.0 — real resamplers put the transition band
below Nyquist, so the kernel is not a pure sinc and its taps are not a unit impulse.
Passthrough has to be an explicit path, and a deadband would mean inferring the mode
from an analog value every block, dragging in threshold width, hysteresis and
flip-flopping; a button puts the transition at one known instant. Keep the resampler
running always and switch which output is selected, so its delay line stays warm.
Cross-fade the handover over 5-10 ms, because switching cold clicks. Touching the jog
suspends unity, and restoring it **snaps the read position to the nearest integer
sample** — bit-perfect is impossible from a fractional offset. Under one sample,
inaudible, and the null test catches its absence.

## Shape of the program

The primary division is by **deadline**, not by phase — insert, browse, play describe
what the user does, not where the seams are. Exactly one thing has a deadline: the
audio callback, which reads the ring and nothing else.

**Mount is not ours** — a udev rule and `systemd-mount`, zero application code.

| | |
|---|---|
| Media watch | `src/media.rs` |
| File layer | `src/sndfile/`, `src/file.rs` |
| Browser | `src/browser.rs` |
| Input | `src/input.rs` |
| Transport | `src/transport.rs` |
| Loaded | `src/loaded.rs` |
| Cue store | `src/cue.rs` |
| Audio engine | `src/window.rs`, `src/ring.rs`, `src/sink/`, `src/app/audio.rs` |
| Dispatch | `src/app/deck.rs` |
| Track lifecycle | `src/app/track.rs` |
| Control loop | `src/app/controls.rs` |
| Display | not written |

**What each module owns is in its own doc comment**, which says it first-hand and
cannot drift from the code. This table is the map, not the description. Four rules
live *between* modules, which is why they are here and not there:

- **A module is not a thread.** The file layer is called from the window thread when
  filling the ring and from the browser when reading a header, and must be safe for
  both without either becoming the other's problem.
- **ENTER acts on the selection, FF/REW on the playing track.** One button's two
  gestures must not address two objects.
- **Every decision happens on the control thread.** The audio thread publishes the
  position and nothing else. The first version reached over.
- **The transport's names are the code's, not the CDJ's.** `Stopped` means *nothing
  loaded*; `is_silent` is the position moving with output muted, which is what makes
  v1's FF/REW a seek. Two of the three were got wrong here before they were named.

## Threading

- **Audio callback** — reads the locked int32 ring, resamples in v2. No malloc, no
  lock, no I/O, no page fault.
- **Window thread** — reads through libsndfile into the ring, converting endianness
  and width on the way. Blocking, allocating and locking are all fine; this is the
  thread the deadline does not reach, and where failures surface, a pulled stick
  included.
- **Control thread** — reads `/dev/input`, converts events to velocity, writes a
  lock-free slot. Same shape for buttons and for a jog, so v2 substitutes rather
  than rewrites.
- **UI and browser** — same process, same language, no deadline. No IPC boundary and
  no second language to cross; PortAudio's own guidance lists crossing language
  boundaries as a hazard in its own right.

Pin the audio thread and its IRQs to different cores from GPIO interrupt handling —
four cores, one deck. Target 5-10 ms output latency for v2 jog response:
`threadirqs`, `SCHED_FIFO` in the 70-80 range, and a PREEMPT_RT kernel likely
unnecessary.

**The period choice is not free, and the bottom of the rate range is where it
binds** — the same frame count is more time at a lower rate:

| at 44.1 kHz | 2 periods | 3 periods |
|---|---|---|
| 128 frames | 5.8 ms | 8.7 ms |
| 256 frames | **11.6 ms** | **17.4 ms** |

So read it as "128 frames, and 256 becomes available from 96 kHz upward", not as a
free choice.

## Display

`embedded-graphics` gives one drawing API behind a `DrawTarget` trait, with drivers
for every controller in play and `linux-embedded-hal` putting them on `/dev/i2c` and
`/dev/spidev`. **The panel model is not locked in by the UI code** — the device
constructor is the only line that changes, and
`embedded-graphics-simulator` runs the same code on a desktop, so geometries can be
compared with real filenames before anything is bought. That reversibility is why
the language choice went the way it did: `luma` gave Python the same property and C
has no equivalent.

**Update on state change, not on a timer** — not for noise, which the isolator
settles, but for bus time. Frame bytes are `width x height x bpp / 8` and both
factors bite. **The panel is on SPI** ([#2](https://github.com/tamatebox/deck-pi/issues/2)),
where a 128x64 frame is 0.82 ms at 10 MHz, so the discipline below is good practice
rather than load-bearing; it was sized against the same frame taking ~26 ms over a
400 kHz I2C bus shared with the WM8804.

One qualification, because the rule as written is too strong: a position readout has
to advance while a track plays, and that *is* a timer. Take it as **full redraws on
state change, and a small position field on a slow tick** — once a second is plenty.
That also keeps the idle timer from firing mid-track, which should be deliberate
rather than a side effect.

Coalesce encoder events and redraw at most every 30-50 ms, or fast scrolling falls
behind. **Header reads ride the same budget**: drive them from what is actually
rendered rather than from each encoder event, and cache by path, so a fast spin
costs one open per redraw and not one per detent.

### If the panel is an OLED

Dim after ~30 s idle and blank after a few minutes; the blank command also stops the
charge pump, so burn-in and power are one timer. **Gate it on the transport**, though:
with a long track playing, "no input for a few minutes" is a *normal* state, and
blanking then would hide the position readout exactly when it is being watched, in a
dark room, during a set. Idle means nothing playing.

**This applies only to the OLED candidates** — an ILI9341 TFT has no burn-in and no
charge pump, but a backlight that wants its own timer instead. Nothing outside this
subsection should assume which it is; several documents used to say "OLED" outright,
which was deciding an open question by wording.

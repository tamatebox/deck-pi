# Architecture

## The one idea

**The boundary that matters is the deadline, not the machine.** The audio callback
reads locked RAM and nothing else. Everything that touches a file, a header, a
byte order or a sample width happens on a thread that is allowed to block.

Draw that line correctly and the work is cheap wherever it runs. Uncompressed PCM
has nothing expensive in it: a byte swap is one ARM instruction, unpacking 24-bit
to 32-bit is a few more, and neither is a decoder. What was never affordable was
doing them *under a deadline*.

That is what makes bit-perfect playback and a 1.2 GHz A53 compatible. (The 3B+ is
specified at 1.4 GHz, but its soft temperature limit drops it to 1.2 GHz at 60 C,
so 1.2 GHz is the sustained figure everything here is sized against — see
`hardware.md`.)

An earlier version of this document drew the line between the Pi and a preparing
machine, and normalised every file offline into headerless raw PCM. That bought
nothing — see `decisions.md`, which records why it was dropped.

## Library

**The USB stick is the library.** It is prepared on another machine, carried, and
plugged in; the deck reads it and never writes it. One stick at a time — the first
block device found — mounted **read-only**, as either **exFAT or HFS+**, the two
filesystems a Mac writes natively that also have mature in-tree Linux drivers. A
journaled HFS+ volume is forced read-only by the driver anyway, which is the same
policy from the other direction. There is no import step and no database: the folder tree is the index.

The Pi holds no library of its own. Its SD card carries the OS and the state the
deck itself creates (see Cue points below).

### What plays

Exactly what the DDC can send, and nothing else:

| | |
|---|---|
| Container | WAV, AIFF, AIFF-C, **RF64, Wave64** |
| Sample rate | 44.1 / 88.2 / 176.4 and 48 / 96 / 192 kHz |
| Bit depth | **int16 and int24 only** |
| Combinations | all **6 x 2 = 12** of them, with no preferred subset |
| Channels | stereo (mono is duplicated to both — lossless) |

`hardware.md` states the interface limit as 44.1-192 kHz, 24 bit max. The software
scope is that limit, so there is no second rule to remember: if the Digi2 Pro can
send it, the deck plays it.

### What does not, and why it is always known in advance

| Rejected | Reason |
|---|---|
| 32-bit int or float | Above the 24-bit output ceiling. Converting float also needs a clipping or scaling decision, and scaling would be a gain stage |
| 8-bit | Below int16; also unsigned by convention, an extra path for nothing |
| Rates outside 44.1-192 kHz (32 kHz, 22.05 kHz) | Outside the interface limit |
| Compressed — MP3, FLAC, AAC | A decoder would have to run on the Pi |
| DSD — `.dsf`, `.dff` | No DoP decoder board is fitted, deliberately (`hardware.md`) |

**Every one of these is decidable from the header alone**, so the browser reads
the header of the highlighted row and can refuse *before* PLAY is pressed. Nothing
in this list can surprise you mid-set.

### What the DAC accepts is not our problem

S/PDIF is unidirectional. The Digi2 Pro sends whatever rate it was given and does
not know what is downstream; a DAC that cannot lock to it goes silent, and nothing
comes back, so the software **cannot detect this, ever**.

That is accepted rather than engineered around. If it does not play, it does not
play. The display already shows the rate and depth in use, so silence next to a
visible "192 kHz" is as much diagnosis as exists or is needed, and where the
downstream is unknown the answer is not to carry rates you cannot guarantee —
converting when building the stick, like everything else the deck refuses.

A configurable "my DAC does up to N kHz" ceiling was proposed here twice and
dropped both times. The deck cannot tell which DAC is attached, so it would be a
claim rather than a fact, and a stale claim asserts a capability that is not there
while sounding certain. Not worth a setting for a failure that is this legible.

Anything on this list that is wanted is converted on the preparing machine, where
there is no deadline and the lossy decisions can be made deliberately. Same
argument as the downconversion note below.

### The three problems, and where they go now

Reading WAV and AIFF directly means three problems. None of them requires an
import step; all three require only that they happen off the realtime thread.

- **AIFF is big-endian**, and ARM is not, so every sample needs a byte swap.
  AIFF-C's `sowt` is the exception — little-endian — so it cannot even be decided
  by container type. → `rev16` / `rev32`, one instruction, in the window thread.
- **24-bit samples are 3 bytes**, so indexing means assembling each sample from
  its bytes rather than reading an array. → unpacked into the int32 ring, in the
  window thread.
- **RIFF and AIFF chunk sizes are 32-bit**, capping a file at 4 GB — and many
  implementations treat the field as signed, so 2 GB is the compatible ceiling.
  → unchanged, and it binds. See Track length ceiling. RF64 and Wave64 lift it.

**libsndfile does all of this, in the window thread.** It already handles
big-endian AIFF, `sowt`, 24-bit unpacking, the 80-bit IEEE extended-precision
sample rate in the AIFF COMM chunk, and RF64. It **must not appear in the audio
callback** — it buffers, allocates and locks — but the window thread has no
deadline, so none of that binds there.

It is bound by a **hand-written FFI**, not a binding crate — see
`implementation.md`.

### Cue points

Imported cues do not exist: the stick is read-only and nothing prepares it with
metadata. Cues are punched on the deck and stored **on the Pi's SD card**, keyed
by the volume's UUID plus the file's relative path. The UUID comes from `blkid` or
the udev environment, not from inside the mount — neither filesystem exposes its
serial through a file API.

The stick is content; the Pi owns state it created. One consequence to accept:
cues would not travel between two decks, if a second is ever built, because they
are two machines.

For a long piece, cues are the primary way to navigate *inside* a track, not just
a mixing tool — which is why cue regions are pre-locked (see Playback).

### Do not upsample

Native rate is kept per track. Converting a 44.1 kHz source to 96 kHz gains
nothing audible, costs 2.2x the space on the stick, costs 2.2x the CPU in v2, and
makes bit-perfect output impossible for that track. If a 192 kHz source is
inconvenient, downconvert it when preparing the stick, with a high-quality SRC —
off the deck there is no time limit, so nothing is lost.

This is the general escape hatch. Anything the deck refuses — 32-bit float, a
compressed file, an out-of-range rate — is converted the same way, in the same
place, for the same reason.

**With existing tools, and not by anything in this project.** `sox` and `ffmpeg`
already do all of it, and both can be built against libsoxr — which grew out of
SoX's own resampler and is the same library v2 will use on the deck. So an offline
downconvert and an on-deck one are the same code, which is a better guarantee than
a bespoke converter would be. Preparing a stick is a workflow step, not a
component: it runs on another machine, shares no code with the deck, and the deck
cannot tell how a file was made.

### Storage

Sources are played as they are, so 24-bit stays packed at 3 bytes per sample —
these are file sizes, not the 1.33x-inflated int32 figures an import step would
have produced. A stereo frame is 4 bytes at int16 and 6 at int24, so the rate is
just `sample rate x frame bytes`.

| | int16 GB/h | int24 GB/h | 256 GB holds (16 / 24) |
|---|---|---|---|
| 44.1 kHz | 0.64 | 0.95 | 403 h / 269 h |
| 48 kHz | 0.69 | 1.04 | 371 h / 247 h |
| 88.2 kHz | 1.27 | 1.91 | 202 h / 134 h |
| 96 kHz | 1.38 | 2.07 | 185 h / 123 h |
| 176.4 kHz | 2.54 | 3.81 | 101 h / 67 h |
| 192 kHz | 2.77 | 4.15 | 93 h / 62 h |

Capacity is not a constraint at any of these for a stick that fits in a pocket.

Bus contention is not one either. The audio path is I2S, not USB, so the stick has
the Pi 3B+'s single USB 2.0 bus effectively to itself: the only other device on it
is Ethernet, and operation is network-independent — the cable can be out during a
set. Sustained read during playback is 1.15 MB/s at the very worst (192/24), which
any stick delivers.

### Track length ceiling

Not RAM — RAM cost is constant in track length (see Playback). The limit is the
**container**: RIFF and AIFF chunk sizes are 32-bit.

At the 2 GiB ceiling:

| | int16 | int24 |
|---|---|---|
| 44.1 kHz | 3 h 23 m | 2 h 15 m |
| 48 kHz | 3 h 06 m | 2 h 04 m |
| 88.2 kHz | 1 h 41 m | 1 h 08 m |
| 96 kHz | 1 h 33 m | 1 h 02 m |
| 176.4 kHz | 51 m | 34 m |
| 192 kHz | 47 m | 31 m |

4 GiB is exactly double each figure, but 2 GiB is the one that matters: many
implementations treat the size field as signed. The risk sits with whatever **wrote** the file, not with reading
it — a tool that runs past 2 GiB into a plain WAV can emit a wrapped size field,
and the file then plays and stops early with nothing to indicate why. The browser
should treat an implausible declared length as suspect.

**RF64 and Wave64 lift the ceiling entirely** (64-bit sizes), and libsndfile reads
them in the window thread at no extra cost. Long-form work above 1 hour at 96/24,
or above half an hour at 192/24, needs one of those containers.

## Playback

**The window thread reads through libsndfile into a locked int32 ring around the
playhead. The audio callback reads the ring and nothing else.**

Samples land in the ring in the **output's own layout**: `S24_LE`, meaning the
24-bit value right-aligned in a 32-bit word. `sf_readf_int` hands back a
left-justified int32 — int16 shifted left 16, int24 left 8 — so filling the ring
is that value shifted right 8. One buffer format, one code path in the callback,
no branch on source depth, and nothing for the callback to convert.

Every step is a pure shift, so the whole chain is lossless: an int24 source
returns as `value << 8` and comes back to itself; an int16 source returns as
`value << 16` and lands in the top 16 bits of the 24-bit field with zeros below,
which is the standard promotion.

**The shift belongs in the window thread, not the callback.** It could legally go
in either — a shift allocates nothing and locks nothing — but the window thread
has no deadline, and moving work off the deadline is the whole organising idea.

`S24_LE` is not a choice; it is the only useful format both drivers offer. See
`implementation.md`.

The float read path normalises to [-1.0, 1.0] instead, and `SFC_SET_SCALE_*` only
affects float-integer conversion. Neither is reachable: sources are int16 or int24
and we call the integer entry point. Excluding 32-bit is what keeps it that way.

The thread keeps the window filled ahead of and behind the playhead; cue regions
are pre-locked so a cold seek does not stall. The callback does no syscall, takes
no lock, allocates nothing, and — because the ring is locked RAM rather than a
file mapping — **cannot fault**.

### Size the window in bytes

`min(60 s, N MiB)`. Time alone would make RAM swing 4x across the supported rates;
a byte cap holds it flat and degrades the window length instead. Shown at
N = 64 MiB, which is illustrative — the value is not yet chosen:

Note there is **no bit-depth axis here.** The ring is int32 whatever the source
was, so the window is a function of sample rate alone — one fewer thing to reason
about, and another reason the uniform ring earns its slightly larger footprint for
int16 material.

| | Ring fill rate | Window at 64 MiB |
|---|---|---|
| 44.1 kHz | 353 kB/s | ±60 s |
| 48 kHz | 384 kB/s | ±60 s |
| 88.2 kHz | 706 kB/s | ±47 s |
| 96 kHz | 768 kB/s | ±43 s |
| 176.4 kHz | 1.41 MB/s | ±23 s |
| 192 kHz | 1.54 MB/s | ±21 s |

A three-hour file costs the same as a three-minute one. **RAM cost is constant in
both track length and sample rate** — the first is what decouples length from the
Pi 3B+'s 1 GB, the second is what lets an unexpected hi-res file play with a
shorter window instead of failing. Pick N from jog feel in v2; v1 only ever reads
forward, so even ±21 s is generous there.

### Why a ring and not mmap

An earlier version mapped the file and mlocked a window of *file pages*, so the
callback could index the mapping directly. That works only when the file is
already in the target format. With a byte swap and a 24-bit unpack in the path a
RAM buffer is needed anyway, which removes mmap's whole advantage — and dropping
mmap pays three times over:

- **A stick pulled mid-playback cannot fault the callback.** Touching a mapping
  whose device is gone raises SIGBUS, and it would have raised it inside the audio
  thread. There are no file-backed pages in the callback's path now.
- **No 32-bit address-space ceiling.** A three-hour 192/24 file is ~16 GB and
  cannot be mapped in a 32-bit userspace at all. Reads with 64-bit offsets work
  either way, so the OS bit width stops being a design input.
- **RF64 and Wave64 come free**, because libsndfile is doing the reading.

Jog is unaffected: the ring is RAM, so reads inside it are free in either
direction, and scrubbing past its edge means an `sf_seek` and a refill — exactly
what a page fault would have cost.

Pulling the stick therefore behaves like a CDJ, and for the same reason: what is
already resident keeps playing. The grace period is the forward half of the
window, so it is bounded rather than guaranteed, and the window thread is where
the failure surfaces — it has no deadline and may fail.

### Position

**float64.** A float32 accumulator has a 24-bit mantissa; past 2^23 samples the
spacing between representable values reaches 1.0. At 44.1 kHz that is ~190 s,
after which the fractional position is gone and interpolation quietly stops
working — audible as the back half of a track degrading, with nothing pointing at
the cause.

### Sample rate

The output rate follows the source, set when the track loads. Reopening the ALSA
device per track is free here: the other deck is a physically separate Pi with its
own DDC, so nothing audible is interrupted. The Digi2 Pro's two oscillators cover
the 44.1 and 48 kHz families exactly, with no fractional division either way.

All six supported rates are one of those two families, so **crossing families
switches oscillators** — via GPIO 5/6, the lines `hardware.md` reserves. The
driver does this itself from `hw_params`; the application only sets a rate and
never touches those pins. It is normal operation, not an edge case: a 44.1 kHz
file followed by a 48 kHz one exercises it.

Staying within a rate costs nothing at all — the machine driver returns early when
the requested rate equals the current one, so a run of same-rate tracks never
reconfigures.

A rate outside 44.1-192 kHz cannot be played at all, in v1 or v2. It is visible in
the header, so the browser refuses on highlight rather than on PLAY — see Library.

### v1 is unconditionally bit-perfect

With no pitch and no jog, there is no second mode. `hw:` device, matched rate, no
resampling, no gain — the source samples reach the DAC untouched.

*Unconditionally* is now literal, and excluding 32-bit is what buys it. Every
supported conversion is a pure shift into the ring's `S24_LE` layout, so there is
no depth, rate or container in scope that costs a bit. A 32-bit float source would
have needed a clipping or scaling decision, and scaling would be a gain stage —
which is why it is out of scope rather than handled.

FF and REW do not break this either: they are a **silent seek** in v1. Position
advances while the button is held and the display follows, but no audio is
produced, so there is never a moment where something other than the source
reaches the DAC. An audible scan would need the resampler, and that is v2, where
it becomes `r = 4` on the rate variable and the unity button already owns the mode
question.

### Holding it on the ALSA side

Four requirements, because ALSA converts silently when asked wrongly:
**`hw:` device only** — `default` converts and usually mixes, `plughw` resamples;
**the exact rate, never the nearest** — the convenience call succeeds at a
different rate and reports nothing; **no software volume**, ALSA's own included;
and **a card with a volume control scales the stream even on `hw:`**, which the
Digi2 Pro avoids by having none at all.

The calls, the flags, and why the null test alone is not enough are in
`implementation.md`.

## v2: pitch and jog

### One rate variable

Pitch and jog are the same mechanism — read at rate `r`. Pitch is `r` near 1.0,
jog is `r` varying per block and going negative, pause is `r = 0`. Write one
resampling read path and all three ride on it.

### libsoxr, not libsamplerate

libsamplerate's three sinc converters all have **97 dB SNR** — they differ only in
bandwidth (97 / 90 / 80 % of Nyquist). 97 dB is roughly 16 bits, so it would cap
the entire system at 16-bit quality whenever pitch is off centre, which is most of
the time in use. That defeats the hi-res sources and the whole output chain.

libsoxr specifies quality in bits — `SOXR_HQ` is 20-bit, `SOXR_VHQ` 28-bit — and
carries two things this application specifically needs:

- **`SOXR_VR`**, a first-class variable-rate mode. Varispeed is a supported
  feature, not a ratio being poked every block.
- **`soxr_set_io_ratio(soxr, ratio, slew_len)`** — the third argument slews the
  rate change across a span, which is the fix for zipper noise at block
  boundaries. It does not have to be hand-written.

`passband_end`, `stopband_begin` and `phase_response` are all directly settable,
so bandwidth trades against CPU continuously rather than in three steps.

**The CPU budget is an estimate, not a measurement.** Extrapolating from A53 IPC,
libsamplerate's SINC_BEST looked like ~2x realtime at 44.1 kHz output and under
1x at 96 kHz. libsoxr should do better, but *should* is not a number. Benchmark
precision x output rate x pitch range on the actual Pi before the board choice is
locked, because the answer decides whether the 3B+ survives into v2.

**Benchmark it thermally soaked.** The 3B+ soft-throttles from 1.4 to 1.2 GHz at
60 C, so a run started from cold measures a clock the board will not hold for a
set. Let it reach steady state first, or pin the clock, and record which. A cold
benchmark would pass a board that fails twenty minutes in.

Note that downsampling costs extra: a 192 kHz source at 96 kHz output costs about
twice a 96 kHz source at 96 kHz. Source rate matters as much as output rate.

### The unity button

Bit-perfect output stops being automatic once a resampler exists, and it cannot be
recovered by feeding the resampler a ratio of 1.0: real resamplers put the
transition band below Nyquist, so the kernel is not a pure sinc and its integer
taps are not a unit impulse. Passthrough has to be an explicit path.

Make it an explicit **button**, not a deadband on the fader. A deadband means
inferring the mode from an analog value every block, which drags in threshold
width, hysteresis, and flip-flopping as the fader drifts across the boundary. A
button puts the transition at one known instant.

- Keep the resampler running always — the CPU budget is sized for the worst case
  anyway — and switch only which output is selected. Its delay line stays warm, so
  either direction can hand over immediately.
- Cross-fade the handover over 5-10 ms. Switching cold clicks: the filter holds
  input samples in its delay line and has its own group delay.
- Touching the jog suspends unity. Restoring it requires **snapping the read
  position to the nearest integer sample** — after scrubbing it is fractional, and
  bit-perfect is not possible from a fractional offset. The jump is under one
  sample (23 us at 44.1 kHz) and inaudible, but it must be deliberate. The null
  test catches its absence.
- Either gate the button on the fader being near centre, or use pickup on
  release. A pitch jump on disengage is worse than either.

Unity requires output rate to equal source rate, and no gain anywhere. Both hold
in this design — a software fader would end it.

## Shape of the program

The primary division is not by phase — insert, browse, play — but by **deadline**,
because that is the only boundary that constrains structure. Exactly one thing has
one:

| | |
|---|---|
| Under a deadline | the audio callback, which reads the ring and nothing else |
| Not | everything else |

Phases describe what the user does, not where the seams are. Of the three, the
first is not code at all, two concerns cut across all of them, and one sits
underneath.

| | |
|---|---|
| **Mount** — *not ours* | A udev rule and `systemd-mount`. Zero application code; see `implementation.md`. |
| **Media watch** | The fixed mount point appearing and disappearing, the volume UUID from blkid, and three states: nothing mounted, mounted but unreadable, browsable. |
| **File layer** | The libsndfile FFI. Opens one file's header, and reads frames into the ring's layout. **Shared** — the browser needs it for length, rate and the four rejections; playback needs it to fill the ring. |
| **Browser** | Walks the folder tree, caches headers by path, holds the selection. The model. |
| **Display** | `embedded-graphics` over one panel driver, the redraw budget, the idle timers. The view. **Cuts across** — it renders browsing and transport alike. |
| **Input** | evdev, the kernel-decoded encoder, tap-versus-hold for FF and REW. **Cuts across**, and deliberately knows nothing about either: it emits standard keycodes, and which GPIO produces which is a line in `config.txt`. |
| **Transport** | The rate variable, the float64 position, and what PLAY / CUE / FF / REW mean. Writes the lock-free slot; never touches the ring. |
| **Audio engine** | The window thread that fills the ring, the callback that drains it, the per-track ALSA setup. |
| **Cue store** | Cues on the SD card, keyed by volume UUID plus relative path. The only state the application persists. |

Threading below is a **different axis**, saying which of these run where and under
what rules. A module is not a thread: the file layer is called from the window
thread when filling the ring and from the browser when reading a header, and it
has to be safe for both without either becoming the other's problem.

## Threading

- **Audio callback** — reads the locked int32 ring, resamples in v2. No malloc, no
  lock, no I/O, and no page fault. Enforce from the first commit, while the load is
  light enough to get away with breaking it.
- **Window thread** — reads through libsndfile into the ring, converting endianness
  and sample width on the way, and keeps it filled ahead of and behind the
  playhead. Blocking, allocating and locking are all fine here; this is the thread
  the deadline does not reach. Failures surface here too, a pulled stick included.
- **Control thread** — reads `/dev/input`, converts events to velocity, writes to
  a lock-free slot the callback reads. Same shape for buttons and for a jog, so
  v2 substitutes rather than rewrites.
- **UI and browser** — same process, same language. Walking the folder tree,
  reading the highlighted row's header and drawing the display all have no
  deadline. There is no IPC boundary and no second language to cross; PortAudio's
  callback guidance lists "crossing language boundaries" as a hazard in its own
  right.

Pin the audio thread and its IRQs to different cores from GPIO interrupt handling;
the 3B+ has four cores and one deck to run.

Target ~5-10 ms output latency for v2 jog response: 128-256 frame periods, 2-3
periods, plus `threadirqs` and `SCHED_FIFO` in the 70-80 range. A PREEMPT_RT
kernel is likely unnecessary.

## Display

`embedded-graphics` gives one drawing API behind a `DrawTarget` trait, and drivers
implementing it exist for every controller in play — ssd1306, ssd1309, ssd1322
(including a 256x64 variant), ssd1327, ili9341, st7789 — with
`linux-embedded-hal` putting them on the Pi's `/dev/i2c` and `/dev/spidev` rather
than on a microcontroller's peripherals. So the display model is **not locked in
by the UI code**: the device constructor is the only line that changes. Prototype
against a cheap 0.96 in panel and pick the real one after seeing actual filenames
on screen.

This is the reversibility that keeps open question 1 open, and it is why the
language choice went the way it did — `luma` gave Python the same property, and C
has no equivalent at all.

Update on state change, not on a timer. Not for noise — the isolator settles that
— but for bus time: a full 128x64 frame is ~26 ms over I2C at 400 kHz, and the
WM8804 shares that bus.

One qualification, because the rule as written is too strong: a position readout
has to advance while a track plays, and that *is* a timer. Take it as **full
redraws on state change, and a small position field on a slow tick** — once a
second is plenty, and a partial update of a few characters costs a fraction of
those 26 ms. It also means the idle timer below never fires mid-track, which is
the wanted behaviour and should be deliberate rather than a side effect.

Coalesce encoder events and redraw at most every 30-50 ms, or fast scrolling falls
behind. **Header reads ride the same budget**: the browser opens the visible rows'
files to show length, rate and depth and to mark the unplayable ones, so drive
those reads from what is actually rendered rather than from each encoder event,
and cache them by path. A fast spin then costs one open per redraw, not one per
detent.

### If the panel is an OLED

Dim after ~30 s idle and blank after a few minutes. The blank command also stops
the charge pump, so burn-in and power are handled by one timer.

**Gate that on the transport, though.** These timers were written for an idle
appliance. With 80-minute tracks, "no input for a few minutes" is the *normal*
state while something is playing, and blanking then would hide the position
readout exactly when it is being watched — in a dark room, during a set. Idle
means idle: nothing playing. While the transport is running, leave it up.

**This applies only to the OLED candidates.** Open question 1 has not been decided,
and one of its options is an ILI9341 TFT — backlit, with no burn-in and no charge
pump, but with a backlight that wants its own idle timer instead. Nothing outside
this subsection should assume which it is; several documents used to say "OLED"
outright, which was deciding an open question by wording.

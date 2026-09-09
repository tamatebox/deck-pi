# Implementation

Rust, one process, one binary — engine, browser and display.

`architecture.md` says what the design is and why. This file says what to type and
what will betray you quietly. It churns as crates and APIs move; the design should
not, which is why they are separate files.

## Holding it on the ALSA side

Bit-perfection is not just a property of our own code — ALSA will quietly convert
if asked wrongly.

- **`hw:CARD=n,DEV=0` only.** `default` brings format conversion and usually
  `dmix`; `plughw` resamples. Neither reports that it did so.
- **`snd_pcm_hw_params_set_rate`, never `set_rate_near`.** The `_near` variant
  picks the closest supported rate and **succeeds**. One function name apart, no
  error, wrong rate — the exact failure shape this project keeps running into.
  Fail the track load instead.

  In the Rust binding this reads worse than it is: the exact setter is
  `set_rate(rate, ValueOr::Nearest)`, where `Nearest` is the `dir` argument
  meaning zero, **not** "pick a nearby rate". The trap is still `set_rate_near`,
  which is simply never called. Confusable enough to be worth the sentence.
- **The `alsa` crate's obvious write call allocates on the audio thread.**
  `pcm.io_i32_s24()` verifies the format through `hw_params_current()`, which calls
  `snd_pcm_hw_params_malloc` — once per period, in the callback's thread. Read in
  alsa-0.12.1's `pcm.rs`. So "use the `alsa` crate" and "the callback allocates
  nothing" are **not jointly satisfiable through the documented-looking call**.

  The way out: verify the format **once at open**, which discharges the safety
  obligation of `unsafe pcm.io_unchecked::<i32>()`, and let the hot path use that.
  `assert_no_alloc` is what catches the mistake if anyone reverts it — which is the
  clearest case yet for having it.
- **`PCM::open_with_flags`** (unsafe, takes the mode bits) is how all four of
  `NO_AUTO_RESAMPLE`, `NO_AUTO_CHANNELS`, `NO_AUTO_FORMAT` and `NO_SOFTVOL`
  actually get passed. The safe `PCM::new` does not take them.
- **`snd_pcm_hw_params_set_rate_resample(..., 0)`** explicitly, and open with
  `SND_PCM_NO_AUTO_RESAMPLE | NO_AUTO_FORMAT | NO_AUTO_CHANNELS | NO_SOFTVOL`.
  On `hw:` there are no plugins to disable, so this is belt and braces — but
  `NO_SOFTVOL` also stops alsa-lib inserting a software volume of its own, which
  the no-gain invariant wants regardless.
- **A card with a volume control means the driver scales the stream**, even on
  `hw:`. The Digi2 Pro has none by design, so the invariant holds in hardware,
  driver and application at once. Confirm with `amixer -c N contents` that there
  is nothing there to scale with.

## The output format is S24_LE

Not a choice, and not something to discover on the hardware — the drivers declare
it statically, so it was read out of the kernel source.

| | Declared formats |
|---|---|
| `bcm2835-i2s.c` (CPU DAI) | `S16_LE`, `S24_LE`, `S32_LE` |
| `wm8804.c` (codec DAI) | `S16_LE`, `S20_3LE`, `S24_LE` |
| **usable intersection** | **`S16_LE`, `S24_LE`** |

`WM8804_FORMATS` does **not** include `S32_LE`, so a 32-bit output frame is not
available. `S24_LE` is a 32-bit little-endian word with the sample right-aligned
in bits 0..23 — which is why the ring is filled with `sf_readf_int`'s value
shifted **right 8**, in the window thread. See `architecture.md`.

Two more facts from the same source, both useful:

- `snd_soc_dai_set_bclk_ratio(cpu_dai, 64)` fixes BCLK at 64x Fs, so the I2S bus
  always carries 32-bit slots per channel. `S16_LE` versus `S24_LE` makes no
  difference to the wire.
- `hw_params` returns early when the requested rate equals the current one, so a
  run of same-rate tracks costs no reconfiguration.

Source: `sound/soc/codecs/wm8804.c`, `sound/soc/bcm/bcm2835-i2s.c` and
`sound/soc/bcm/rpi-wm8804-soundcard.c` in <https://github.com/raspberrypi/linux>.
Read from `rpi-6.18.y`. With **Raspberry Pi OS Lite (64-bit)** now the chosen base,
whatever kernel that image ships will be older, so this stops being a general
caution and becomes a concrete first-boot task, alongside `alsacap` and
`amixer -c N contents`:

- confirm `WM8804_FORMATS` still excludes `S32_LE`
- confirm the overlay still names `clock44-gpio` and `clock48-gpio`
- confirm `snd_soc_dai_set_bclk_ratio(cpu_dai, 64)` still holds

## Testing it from both ends

Two tests, and they check different halves. Neither is sufficient alone.

- **Null test — the software half.** Play a file, collect the buffers handed to
  ALSA, and check them against the source. This proves our read path, the ring
  and the left-justification.

  **Assert the ring's absolute values, not a byte round trip.** "Re-encode the
  buffer and compare bytes with the source" is the obvious form and it is
  **symmetric**, so a symmetric error cancels: with a logical shift where the
  arithmetic one belongs, an int24 sample loses its sign bits going in and has
  them restored coming out, and the bytes match. Measured on the real suite —
  the byte-equality form caught 4 of the mutations, the absolute form caught 5.

  So decode the source's data chunk independently — a few lines that do not
  call libsndfile — and assert each ring sample equals
  `source << left_justify_shift >> 8`. Keep the byte round trip as well; it is
  the property the DAC depends on, just not the one that catches this. And
  assert the sign separately: any signal without negative samples passes a
  logical shift.
Two of these are now code rather than instructions: `read_proc_hw_params` parses
the file below and `verify_in_force()` fails naming the field that differs, and
`assert_no_mixer_controls(card)` enumerates the card's mixer and fails if anything
is there — the `amixer` check. Both are one-shot at start and neither runs on the
audio thread. `alsacap` is still the manual part.

- **`hw_params` — the hardware half.** While playing, read
  `/proc/asound/card0/pcm0p/sub0/hw_params`. It reports the rate, format, channel
  count and access mode actually in force. This proves ALSA accepted what we asked
  for and substituted nothing.

Use `alsacap` during bring-up to enumerate what the Digi2 Pro actually offers —
supported rates, formats, and buffer and period ranges — rather than assuming the
datasheet's six rates all appear.

## Reading files

libsndfile, in the window thread, through a **hand-written FFI** rather than a
binding crate. The surface needed is small:

```c
typedef struct { sf_count_t frames; int samplerate; int channels;
                 int format; int sections; int seekable; } SF_INFO;

SNDFILE    *sf_open(const char *path, int mode, SF_INFO *sfinfo);
int         sf_close(SNDFILE *);
sf_count_t  sf_seek(SNDFILE *, sf_count_t frames, int whence);
sf_count_t  sf_readf_int(SNDFILE *, int *ptr, sf_count_t frames);
const char *sf_strerror(SNDFILE *);
```

About forty lines of `extern "C"`. The published Rust bindings stopped moving in
2021; at this size owning the declarations is sturdier and auditable, and the C
library is healthy and packaged everywhere.

`sf_readf_int` is also why the byte swap and the 24-bit unpack are not our code.
Its documented convention puts the source's most significant bit at the
destination's most significant bit, so both happen inside a call we were making
anyway, and both arrive left-justified in an int32.

The one thing we do add is a **shift right 8**, because the output is `S24_LE` and
that is right-aligned. It runs in the window thread while filling the ring, so the
callback has nothing to convert.

**Do not reach for the float entry point.** It normalises to [-1.0, 1.0], and
`SFC_SET_SCALE_*` only affects float-integer conversion. Sources are int16 or
int24, so the integer path is the only one needed and the only one that is
lossless. Excluding 32-bit is what keeps it unreachable.

Format acceptance is one field: `SF_INFO.format & SF_FORMAT_SUBMASK` against
`SF_FORMAT_PCM_16` and `SF_FORMAT_PCM_24`. Everything else is refused — see
`architecture.md` for the list, and for why every rejection is decidable from the
header alone.

## Mounting the stick

Nothing mounts removable media on a headless box: the kernel creates the block
device and stops. `systemd-mount` is the documented answer, and its own man page
gives a udev example, so the pattern is not improvised. It creates a transient
`.mount` unit, which is what makes teardown on removal automatic — calling `mount`
from `RUN=` instead would leave the mount unowned.

Two udev properties exist precisely for what this build needs:

| | |
|---|---|
| `SYSTEMD_MOUNT_WHERE=` | the mount point, instead of the generated `/run/media/system/<label>` |
| `SYSTEMD_MOUNT_OPTIONS=` | the options, when `--options=` is not passed |

So the fixed path and the per-filesystem options both come from udev, with nothing
hand-rolled. Draft — **verify every spelling on the actual image**, the same caution
`hardware.md` applies to overlay parameters. `/media/stick` and `uid=1000` are
placeholders, chosen here and not decided anywhere:

```
# /etc/udev/rules.d/99-deck-stick.rules
ACTION!="add",                   GOTO="deck_end"
SUBSYSTEMS!="usb",               GOTO="deck_end"
SUBSYSTEM!="block",              GOTO="deck_end"
ENV{ID_FS_USAGE}!="filesystem",  GOTO="deck_end"

ENV{ID_FS_TYPE}=="exfat",   ENV{SYSTEMD_MOUNT_OPTIONS}="ro,nosuid,nodev,noexec,uid=1000,gid=1000,fmask=0133,dmask=0022,iocharset=utf8"
ENV{ID_FS_TYPE}=="hfsplus", ENV{SYSTEMD_MOUNT_OPTIONS}="ro,nosuid,nodev,noexec,uid=1000,gid=1000,nls=utf8"

ENV{SYSTEMD_MOUNT_OPTIONS}=="?*", ENV{SYSTEMD_MOUNT_WHERE}="/media/stick", \
  RUN{program}+="/usr/bin/systemd-mount --no-block --automount=no --collect $devnode"

LABEL="deck_end"
```

Why each piece:

- **`ID_FS_TYPE` branching, not `-t auto`.** The two filesystems take different
  option names, so the type has to be known when the options are chosen. It also
  does the partition selection for free: a Mac drive initialised with a GUID
  partition map carries a vfat EFI System Partition, and matching only exfat and
  hfsplus skips it. "The first block device" would have mounted that. A flash stick
  formatted exFAT is often a single MBR partition instead, where the question does
  not arise — but the rule has to survive both.
- **`--automount=no`.** Automount is implied for removable devices, and it makes
  the mount point exist whether or not media is present — which destroys the
  simplest presence test. With it off, the path appears only when something is
  mounted, so "No USB" versus the browser is one `stat`.
- **`--no-block`** because udev rules must not wait, and **`--collect`** so failed
  transient units do not accumulate and need `systemctl reset-failed`.
- **Numeric uid.** Both drivers parse it with `fsparam_uid`; a username does not
  resolve in the kernel.

### Mount options, from the drivers

`fs/exfat/super.c` declares `uid`, `gid`, `umask`, `dmask`, `fmask`,
`allow_utime`, `iocharset`, `errors`, `discard`, `keep_last_dots`, `sys_tz`,
`time_offset`, `zero_size_dir`. `utf8`, `debug`, `namecase` and `codepage` are
marked deprecated — use `iocharset`.

`fs/hfsplus/options.c` declares `creator`, `type`, `umask`, `uid`, `gid`, `part`,
`session`, `nls`, `decompose`/`nodecompose`, `barrier`/`nobarrier`, `force`.
There is no `fmask` or `dmask`, only `umask`.

Two HFS+ traps:

- **Never pass `force`.** It exists to write to journaled or locked volumes. The
  driver otherwise forces read-only on a journaled volume, which is the policy
  here anyway.
- **Never pass `nodecompose`.** `hfsplus_uni2asc` composes filenames on the way
  out by default. Turn that off and every Japanese dakuten becomes a second code
  point, which breaks both display and the per-line character budget.

And one place the documentation misleads: `hfsplus.rst` says `uid=`/`gid=` apply
to files "that have uninitialized permissions structures". The code says otherwise
— `hfsplus_get_perms` overrides unconditionally when the option is present:

```c
i_uid_write(inode, be32_to_cpu(perms->owner));
if ((test_bit(HFSPLUS_SB_UID, &sbi->flags)) || (!i_uid_read(inode) && !mode))
        inode->i_uid = sbi->uid;
```

That matters: a Mac-written volume carries that Mac's uids, and without the
override a file could mount unreadable. `umask=` really does only apply to
uninitialized modes, but with `uid=` in place the on-disk owner bits are the
application's own, so it does not need to.

## Process setup

The callback rules are not only about what the callback does; the process has to
be set up so they can hold.

- `mlockall(MCL_CURRENT | MCL_FUTURE)`, **and pre-fault the stack and heap**.
  Locking future pages is not enough on its own — a page touched for the first
  time inside the callback still faults.
- Open `memlock` and `rtprio` in `/etc/security/limits.conf`. Without this,
  `SCHED_FIFO` and `mlockall` fail at runtime rather than at build time.
- No locks in the callback at all, so priority inversion cannot arise and
  `PTHREAD_PRIO_INHERIT` is not needed.

The rules themselves are wider than "no malloc, no lock, no I/O". Also excluded:
anything worse than O(1), anything whose working set varies, and any third-party
call that does not promise realtime behaviour — which is the general form of the
reason libsndfile stays in the window thread.

## Enforcing the callback rules

`CLAUDE.md` requires the callback discipline to hold *from the first commit*,
while the load is still light enough to get away with breaking it. That is a
statement about intent unless something checks it, and something can:

- **`assert_no_alloc`** — a global allocator wrapper that makes an allocation
  inside a marked region fail loudly. There is no equivalent in C without hooking
  `malloc` by hand, and this is the single strongest practical argument for the
  language choice.
- **`rtrb`** — single-producer single-consumer, lock-free *and* wait-free, fixed
  capacity allocated once at construction. This is the **control-thread slot**.

  It is **not** the ring, and cannot be. `architecture.md` requires reads inside
  the window to be "free in either direction" and the window to be filled ahead
  of *and behind* the playhead; an SPSC FIFO's consumer only moves forward, and
  what it has read is gone. v1 alone would be satisfied by a FIFO — playback
  reads forward and FF/REW are a silent seek — which is exactly the trap: it
  would work now and make v2's jog a rewrite instead of a substitution.

  The ring is a fixed allocation of **`AtomicI32` slots accessed `Relaxed`**,
  addressed by track frame index modulo capacity. Making the slots atomic is
  what makes a concurrent read sound by construction rather than by argument,
  and it is free on the target: a relaxed 32-bit atomic load or store on AArch64
  is a plain `ldr` / `str`, no barrier and no lock instruction. The resident
  span is published as `start`, `end` and a `generation` counter; the callback
  loads `end` first and `start` last, which can only understate what is
  resident, then re-checks both after copying and reports a miss rather than
  emitting a stale or torn sample.
- **`thread-priority`** / **`audio_thread_priority`** — the `SCHED_FIFO` plumbing.

One language-specific hazard to know: dropping the last `Arc` to a buffer **inside
the callback** frees memory on the audio thread. Keep ownership outside the
realtime thread; `basedrop` exists to defer such frees, but not needing it is
better.

## Dependencies

Chosen against one question: **what happens if this goes unmaintained?** Download
counts are cumulative and inflated by CI and transitive use, so they say "this is
in the dependency graph of popular things", not "many people use it directly".
Useful as a health signal, not as a popularity one.

| | Role | Health |
|---|---|---|
| `alsa` | Output | 21M, current — but see the allocation trap above |
| `rtrb` | Control slot only — **not** the ring, see above | 11M, current |
| `thread-priority` | `SCHED_FIFO` | 11M, current |
| `embedded-graphics` | Drawing API | 2.6M, current |
| `linux-embedded-hal` | Panel drivers onto `/dev/i2c`, `/dev/spidev` | 5.9M, current |
| `assert_no_alloc` | Enforcement, not runtime | 4.3M but stale since 2021 |
| panel driver | One, chosen after open question 1 | thin, varies by controller |
| libsndfile | Reading, via hand-written FFI | C library healthy; no crate dependency |
| libsoxr | v2 resampling, hand-written FFI | C library static since 2023 |

What matters is that risk sits in the right places. The crates that are **hard to
replace are the healthy ones**; the ones that are stale or thin have surfaces small
enough to own — a libsndfile FFI is forty lines, a panel driver is an init sequence
and a `DrawTarget` impl, and `assert_no_alloc` is a build-time tool whose failure
costs enforcement rather than function. `evdev` is convenient but not required at
all: an input event is a fixed 24-byte struct.

The reversibility that keeps open question 1 open rests on `embedded-graphics`,
which is healthy — not on any individual panel driver, which is what a naive read
of the download counts would worry about.

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

## What reads as handled and is not

The recurring defect here is not a wrong line. It is something that **reads as
verified** — typed, documented, named in a design document, sometimes tested —
and is not. A five-way review and the fixes after it turned up enough instances
to sort into shapes, and **the shapes matter more than the list, because each
needs a different check and the cheap one clears most of them.**

| | Shape | The check that finds it | Instance |
|---|---|---|---|
| 1 | Declared, never reached | grep **construction sites**, not definitions | `BrowseError::OutsideRoot`, `verify_in_force`, `Transport::reached_end`, `Command::Relocate` |
| 2 | Wired, unreachable **for its stated cause** | ask whether the *stated cause* can reach the site | `window::Event::Failed` — two construction sites, neither reachable by a pulled stick |
| 3 | A one-way door | ask which transitions lead **into** each state | `State::Stopped` — constructed, never stored. Was "pending [#14](https://github.com/tamatebox/deck-pi/issues/14)"; #14 has landed and `Loaded::unload` exists, so what is missing is now the app loop that would wire it to `Transport` |
| 4 | An unstated premise, true of the code and false of the hardware | name the cardinality the code chose, where no document states one | `Device::wait` polled 1 of the 7 nodes `config.txt` creates — most buttons dead |
| 5 | Correct only because something else chose to behave | ask what the code relies on the other side *choosing* to do | `wait` ignored `revents`; a hung-up fd spun 340,838 times in 200 ms, hidden because real evdev returns `ENODEV` |
| 6 | Partial by physics | ask whether the job is as large as the problem | absolute-axis rollover folds correctly; the clamped case emits no event at all, so nothing is recoverable |
| 7 | **Absence of a complaint read as evidence** | break the thing on purpose and confirm the check complains | a linter whose error went to stderr and whose silence was read as a pass; a symmetric null test; a race harness reaching `Overrun` 8.6M times and detecting nothing; a guard whose enforcement depended on **linkage** — an integration test that never touches the crate gets no `#[global_allocator]`, so every `assert_no_alloc` in it silently passes |
| 8 | True under a reading nobody would take | read your own sentence as a stranger, not as its author | "no code path stores this", written beside the constructor |

Three of these need more than a row.

**Shapes 2 and 3 are cleared by the check that catches shape 1**, which is the
whole reason to separate them. `Event::Failed` greps clean — a type, a doc
comment, two construction sites, and an affirmative answer to "is this
reachable?" — because one site sat behind `Command::Relocate`, which nothing
sent, so the deadness was *inherited* rather than local. `State::Stopped` greps
clean for the opposite reason: the constructor is right there, and what is
missing is a transition back.

**Shape 4 has a wrong repair that looks right.** Waiting on each of the seven
nodes in turn spends the full timeout on each, so the round trip becomes seven
poll intervals and `Decoder::tick` runs that much later — dead buttons traded
for a drifting hold threshold. It has to be one `poll` over all of them.

**Shape 7 is the one to internalise, because it invalidates evidence rather
than code.** In each instance nothing complained, and nothing complaining was
taken as a result: a check that never ran, a check that was symmetric so the
error cancelled, a probe optimised out because its allocation was unused, a
harness with no measured sensitivity, a guard that was present in the source and
absent from the binary. **The only way to know a check works is to
make it fail** — remove the fix and confirm the test goes red. Where that is
probabilistic, the detection rate is itself a measurement: this repository's
ring-race guard was measured at 1 detection in 5 runs on macOS and 1 in 10 on
Linux, so a clean run is the *expected* outcome with the bug present, and three
clean runs was not the evidence it read as.

It recurs while being written down. The commit that hardened the argument
parser closed one route to an unpinned realtime run and tested it, and left the
duplicate-flag route beside it unchecked — six passing parser tests standing in
for a parser nobody had tried to break, in the change that documents this shape.

### Two rules that cut across all eight

**Agreement between a comment and its code is evidence about the comment and
never about the code.** Several of these survived because the type, the doc
comment and the design document agreed with one another, and all three were
wrong together.

**An unexercised branch predicts unexercised *consumers*.** `Miss::Relocated`
was recorded as a coverage gap; that was also a prediction. When the `relocate`
seqlock made it routine, two consumers were found to have been wrong all along —
one sending every other variant to `panic!`, the other to a `break` that ended
playback, both treating "the window moved, come back" as a fault. Nothing had
ever asked them. When a dormant branch is made live, **audit what receives it in
the same change**; the branch working is not the question.

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
placeholders, chosen here and not decided anywhere —
[#11](https://github.com/tamatebox/deck-pi/issues/11):

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
  mounted.

  **The presence test is still not existence**, though, and this bullet used to
  say it was. Whether the *directory* is there additionally depends on systemd
  removing it on unmount, and a failed unit or a stray `mkdir` leaves it behind —
  at which point an existence test reports a stick that is not there. `src/media.rs`
  compares the path's `st_dev` with its parent's instead: measured at 79 against 76
  mounted, 76 against 76 with the directory left behind. One extra `stat`, and the
  assumption goes away.
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
be set up so they can hold. **This is `src/rt.rs` now, not instructions** — and it
is `deck-pi --rt-check` on the bring-up CLI, which applies the setup and prints
what the kernel granted.

- `mlockall(MCL_CURRENT | MCL_FUTURE)`, **and pre-fault the stack**. Locking future
  pages is not enough on its own — a page touched for the first time inside the
  callback still faults.
- Open `memlock` and `rtprio` in `/etc/security/limits.conf`. Without this,
  `SCHED_FIFO` and `mlockall` fail at runtime rather than at build time.
- No locks in the callback at all, so priority inversion cannot arise and
  `PTHREAD_PRIO_INHERIT` is not needed.

**Every one of the three calls fails silently, which is why each is read back.**
`mlockall` without the limit returns `ENOMEM` and the process runs unlocked;
`sched_setscheduler` without it returns `EPERM` and the thread runs at normal
priority; `sched_setaffinity` to a core that does not exist returns `EINVAL` and
the thread runs unpinned. Audio comes out in all three cases, and on an idle desk
it comes out fine. So `rt::apply` verifies against `sched_getscheduler`,
`sched_getparam`, `VmLck` in `/proc/self/status` and `sched_getaffinity`, and fails
naming the field — the same discipline as `verify_in_force` for `hw_params`.

Two of the three can be answered *before* trying, from `getrlimit`, which is better
than an errno: it prints the missing `limits.conf` line rather than a number.
Measured in a Linux/aarch64 container, all four paths:

| | Result |
|---|---|
| no privileges | refused, naming `rtprio 80` and `memlock unlimited` |
| `--ulimit memlock=8M` | refused **before** the syscall, with both figures |
| `rtprio=99`, `memlock=-1` | `SCHED_FIFO` 75, `VmLck` 8,560 kB |
| `--rt-check=2` | affinity reads back as `[2]` |
| `--rt-check=999` | `sched_setaffinity` refused, `EINVAL` |

**Two corrections to what this section used to say.**

**The heap does not need pre-faulting**, and saying it did invited code that
pretends to do something. The callback allocates nothing, so it touches no new heap
page; the one large heap object in the audio path is the ring, and the window thread
writes every byte of it while filling, which faults it off the deadline. Growing
glibc's arena and hoping `free` does not hand it back is where the withdrawn
`mallopt` note came from.

**The stack pre-fault has to recurse, and it has to measure itself.** A loop over
one local array touches the same page every time and prefaults nothing — that was
the first version, and it leaves the invariant reading as satisfied: the call is
there, it returns, the pages are untouched. `rt::prefault_stack` therefore returns
the stack depth it actually reached, and `apply` refuses if the reach is less than
half the request. Measured at 266,244 bytes reached for a 262,144-byte request in
`--release` on aarch64, so `write_volatile` plus `black_box` survives optimisation —
which is the thing that had to be checked, not assumed.

The rules themselves are wider than "no malloc, no lock, no I/O". Also excluded:
anything worse than O(1), anything whose working set varies, and any third-party
call that does not promise realtime behaviour — which is the general form of the
reason libsndfile stays in the window thread.

## `overflow-checks = true` in release, and what it trades

`Cargo.toml`'s release profile turns arithmetic overflow checks **on**, which is
not the default and is not a leftover. It was undocumented until it was found by
review, so the reason is written here rather than inferred from the line.

The trade is between two failures in the audio callback. A wrapped index reads
the wrong part of the ring and emits samples that are plausible and wrong —
silent, and exactly what this project fears most. A checked overflow panics, and
the deck stops loudly. **A stop you can hear beats audio you cannot audit**, so
the checks stay on.

**It does not weaken the "cannot fault" invariant, and the two must not be
conflated.** `CLAUDE.md`'s no-fault rule is about **page faults**: it is the
property that dropping mmap bought, so that a stick pulled mid-set cannot raise
SIGBUS inside the audio thread. A panic is a different thing entirely — it needs
no page, no device and no mapping. Nothing about `overflow-checks` touches the
page-fault property, and writing "the callback can now fault" would give away a
claim that is currently exactly true.

Cost is a compare and a branch per arithmetic op, on a path that is already
dominated by the copy out of the ring. Not measured, because nothing has run on
hardware; if it ever matters, measure before turning it off.

## Enforcing the callback rules

`CLAUDE.md` requires the callback discipline to hold *from the first commit*,
while the load is still light enough to get away with breaking it. That is a
statement about intent unless something checks it, and something can:

- **`assert_no_alloc`** — a global allocator wrapper that makes an allocation
  inside a marked region fail loudly. There is no equivalent in C without hooking
  `malloc` by hand, and this is the single strongest practical argument for the
  language choice.

  **It has one blind spot, and v2 puts its heaviest work inside it.** The wrapper
  is Rust's `GlobalAlloc`; a C library calling glibc directly is invisible to it.
  Confirmed from the shared objects' undefined symbols — libsoxr imports
  `malloc@GLIBC_2.17`, `calloc`, `realloc`, `free`. The blind spot is exactly the
  two C dependencies: for **libsndfile** it costs nothing, because it runs in the
  window thread where allocation is allowed anyway; for **libsoxr** it is the
  callback in v2. So the claim above holds for Rust code — which is all of v1's
  callback — and stops being an enforcement story the moment libsoxr arrives.
  Checking *that* needs an `LD_PRELOAD` interposer counting glibc's allocators, not
  `assert_no_alloc`.

  This is not hypothetical even though the resampler turns out to be allocation-free
  when configured correctly: it is *misconfiguration* that allocates, and the blind
  spot is precisely what makes misconfiguration silent. See **Declare the pitch range
  when creating the resampler**.
- **`rtrb`** — **named here during design and never used.** It was to be the
  control-thread slot: single-producer single-consumer, lock-free *and*
  wait-free, fixed capacity allocated once at construction. The slot was built
  instead from plain atomics on `Transport` — a rate, a state, a position, a
  seek request, each a single word — which needs no queue at all, so the
  dependency was never added and `Cargo.toml` has no `rtrb`.

  **The argument for why it could not have served the ring is kept, because it
  is what stops the ring becoming a FIFO later.** `architecture.md` requires reads inside
  the window to be "free in either direction" and the window to be filled ahead
  of *and behind* the playhead; an SPSC FIFO's consumer only moves forward, and
  what it has read is gone. v1 alone would be satisfied by a FIFO — playback
  reads forward and FF/REW are a silent seek — which is exactly the trap: it
  would work now and make v2's jog a rewrite instead of a substitution.

  The ring is a fixed allocation of **`AtomicI32` slots accessed `Relaxed`**,
  addressed by track frame index modulo capacity, and it is cheap on the
  target: a relaxed 32-bit atomic load or store on AArch64 is a plain `ldr` /
  `str`, no barrier and no lock instruction.

  The resident span is published as `start`, `end` and a `generation` counter.
  The callback loads `generation`, then `end`, then `start`; copies; then
  re-loads `start` and `generation` — **not `end`**, which only grows and so
  can never invalidate what was just read. Within one generation that order
  understates what is resident rather than overstating it. **Across a
  relocation it does not**, which is why `relocate` is a seqlock: the
  generation is bumped twice and is odd while the relocation is in progress,
  and the reader refuses an odd generation outright.

  **Two `fence` calls carry the invalidate direction, and they are
  load-bearing.** The `Release`/`Acquire` pair is oriented for *publishing* —
  fill the slots, then store `end` — and invalidating runs the other way with
  no pairing of its own. Do not remove them as redundant with the orderings
  already there: that is precisely the reasoning that left them out, and it
  cost **18 corrupt `Ok`s in 90.6M reads**. An earlier version of this
  paragraph claimed the load order "can only understate what is resident" and
  that atomic slots made the read "sound by construction" — both were unscoped,
  and the code matched the prose while neither matched reality.
  **`src/ring.rs`'s module doc is the authority here**, with the full argument
  and the measurements; this paragraph follows it.
- **`libc`** — the `SCHED_FIFO` plumbing, and the other two calls with it.
  `thread-priority` was the crate named here, and it is not used: two of the three
  calls (`mlockall`, `sched_setaffinity`) are not in it, and the verification needs
  the raw ones regardless — so it would have been a second dependency wrapping one
  of the four calls `src/rt.rs` already makes directly.

One language-specific hazard to know: dropping the last `Arc` to a buffer **inside
the callback** frees memory on the audio thread. Keep ownership outside the
realtime thread; `basedrop` exists to defer such frees, but not needing it is
better.

## Declare the pitch range when creating the resampler

v2 only, and it is a **requirement** rather than a note, because getting it wrong
allocates on the audio thread and nothing tells you.

`soxr_create(input_rate, output_rate, ...)` is what sizes `SOXR_VR`'s internal
buffers. Give it 1:1 and then hand `soxr_set_io_ratio` anything else, and
`soxr_process` reallocates to grow into the range it was not told about — for as long
as the ratio keeps moving. Declare the span and it allocates **nothing**.

Measured over 200,000 periods (1,161 s) on one trajectory sweeping 0.90 to 1.10, the
full range warmed outside the measurement in every run:

| `soxr_create` | Allocating events | Reallocs | Bytes |
|---|---|---|---|
| `(1, 1)` — ratio 1.0 declared | 1,776 | 3,660 | 2.73 GB |
| `(1, 1.10)` — upper bound only | 114 | 228 | 17.5 MB |
| **`(0.90, 1.10)`** — the whole span | **0** | **0** | **0** |
| `(1.10, 1)` — bounds reversed | 1,776 | 3,660 | 2.73 GB |

**Zero here is not a resampler doing nothing**, which is what it would also look
like. Both configurations produce identical output at every ratio — out/in of 1.0851,
1.0280, 0.9766, 0.9301 and 0.8878 at ratios 0.90 through 1.10, matching input/ratio
with the same ~240-frame delay-line offset. The declared-range build resamples
correctly and identically; it simply does not allocate.

So `SOXR_VR` is usable from the callback and the invariant holds. The pitch range is
known before the resampler is built — `decisions.md` fixes it at ±10% — so this is a
configuration requirement, not a constraint.

**The failure shape is this project's usual one, which is why it is a requirement.**
Misconfigure it and the audio is *correct*, the allocation is unbounded, it is on the
audio thread, and **`assert_no_alloc` cannot see it** because libsoxr calls glibc
directly. Three of the four signals this project relies on are silent. It was found
only by an `LD_PRELOAD` interposer counting glibc's allocators.

**And know why it was got wrong the first time**, because the next reader will hit the
same wall: `soxr.h` documents variable-rate creation only as "see example # 5". With
the example not to hand, the create call was **guessed** — 1:1, which looks like the
neutral choice and is the worst one. If the example is still unavailable, verify with
an interposer rather than inferring from the header.

`mallopt(M_MMAP_MAX, 0)` and `M_TRIM_THRESHOLD, -1` were once proposed here to keep
glibc's arena out of `mmap` under `MCL_FUTURE`. **Withdrawn** — correctly configured
there is no allocation to keep anywhere.

**Retracted from this file:** an earlier version carried a large body of measurement
concluding that `SOXR_VR` allocates unboundedly while the ratio moves, with per-event
costs, a saturation table and three usage regimes. All of it described a resampler
created with a 1:1 ratio. The figures were internally consistent and arithmetically
correct, which is exactly why checking them could not catch it — they were right
about the wrong thing. `decisions.md` records the reversal.

## Dependencies

Chosen against one question: **what happens if this goes unmaintained?** Download
counts are cumulative and inflated by CI and transitive use, so they say "this is
in the dependency graph of popular things", not "many people use it directly".
Useful as a health signal, not as a popularity one.

| | Role | Health |
|---|---|---|
| `alsa` | Output | 21M, current — but see the allocation trap above |
| `alsa-sys` | Pulled in directly for the four open-mode flags `PCM::new` cannot pass | tracks `alsa`; same maintainers |
| ~~`rtrb`~~ | **Not a dependency.** Named during design for the control slot, which was built from plain atomics instead — see above | n/a |
| `libc` | `mlockall`, `sched_setscheduler`, `sched_setaffinity` and the read-backs | 400M+, current |
| `embedded-graphics` | Drawing API | 2.6M, current |
| `linux-embedded-hal` | Panel drivers onto `/dev/i2c`, `/dev/spidev` | 5.9M, current |
| `assert_no_alloc` | Enforcement, not runtime | 4.3M but stale since 2021 |
| panel driver | One, chosen after open question 1 | thin, and it varies a lot: `ssd1309` 13k / 2023, `ssd1327` 1.7k / **2020** |
| libsndfile | Reading, via hand-written FFI. **`build.rs` requires >= 1.0.28**, which is where RF64 read support arrives — relax that pin and the Track length ceiling argument in `architecture.md` goes with it, silently | C library healthy; no crate dependency |
| `pkg-config` | Build-dependency. `build.rs` uses it to resolve libsndfile on both the development Mac and the Pi, so the link line is not hardcoded | 300M+, current |
| libsoxr | v2 resampling, hand-written FFI | C library static since 2023 |

What matters is that risk sits in the right places. The crates that are **hard to
replace are the healthy ones**; the ones that are stale or thin have surfaces small
enough to own — a libsndfile FFI is forty lines, a panel driver is an init sequence
and a `DrawTarget` impl, and `assert_no_alloc` is a build-time tool whose failure
costs enforcement rather than function. `evdev` is convenient and is not used: an
input event is a small fixed-layout struct, and `src/input.rs` reads it directly.

**"A fixed 24-byte struct" was not quite right, and the code no longer says it.**
Measured against `linux/input.h` on aarch64: `sizeof(struct input_event)` is 24 with
`type` at 16, `code` at 18 and `value` at 20 — 24 **because `timeval` is 16 on a
64-bit build**. A 32-bit userspace makes `timeval` 8 bytes and the struct 16. So the
figure is a property of the base `decisions.md` chose rather than of the struct, and
the layout is derived from `size_of::<libc::timeval>()` instead of hardcoded.

The reversibility that keeps open question 1 open rests on `embedded-graphics`,
which is healthy — not on any individual panel driver, which is what a naive read
of the download counts would worry about.

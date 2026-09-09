# Decisions

Design settled 2026-09-09. No code exists yet.

## Settled

| Decision | Why |
|---|---|
| Raspberry Pi 3B+ | Already owned. Sufficient for v1 (no DSP at all); v2 is the open question. Same footprint and header as a 3B, so nothing in the board stack depends on which it is. |
| One Pi per deck | Makes per-track output-rate switching free — reopening this deck's DDC cannot interrupt the other deck, which is a different machine. |
| Digi2 Pro + IsolatorPi III | Dual oscillators in master mode remove the Pi's PLL jitter from the clock path; the isolator keeps the Pi's ground noise off the audio boards and gives the controls a non-isolated header of their own. |
| Separate clean supply on J1 | The isolation is only real if the audio side is powered independently. J1 takes 3.3-5 V; it must be 5 V here because the rail passes through to a 5 V HAT. |
| Fit the isolator last, not first | The IsolatorPi III manual (§J-1) says to prove the hardware and software produce audio *before* inserting the isolator. Direct-mounted Digi2 Pro runs the entire v1 software stack, with the clean-supply question deferred — but is not an audio-quality baseline. |
| WAV / AIFF sources only | Uncompressed, so no decoder ever runs on the Pi. RF64 and Wave64 too, which libsndfile reads at no extra cost and which lift the 2 GiB container ceiling. |
| Native rate and depth per track | Upsampling costs space on the stick and CPU in v2 and buys nothing; matching rates is also what makes bit-perfect output possible. |
| Locked int32 ring around the playhead | Decouples track length **and** sample rate from the 1 GB of RAM — cost is constant in both. Was mmap + mlock of file pages; see Reversed for why that changed. |
| Wireless off, Ethernet only | Fewer interrupts, and the 3B+ radio is dual-band, so this removes a 5 GHz transmitter as well as the 2.4 GHz one. Confirm wired access *before* disabling. |
| Buttons and encoders on GPIO, kernel-decoded | No MIDI jitter, no USB polling interval. `gpio-key` / `rotary-encoder` overlays, never userspace polling. |
| No LEDs | Keeps the GPIO wiring simple. Not a noise decision — a statically driven LED is DC and quieter than the display. The display shows state instead, and can show *why*, not just *that*. |
| **Everything in Rust, one process** | v1 would be comfortable in Python but v2 would not, so the engine was never going to be Python. The *split* then collapsed: the browser needs libsndfile too (to read the highlighted row's header), so a language boundary would mean binding it twice or asking the engine over IPC. Rust also has what kept the UI in Python — `embedded-graphics` gives luma's device-swap reversibility, which C has no equivalent for. |
| Enforce the callback rules with `assert_no_alloc` | `CLAUDE.md` requires the discipline from the first commit, which is intent unless something checks it. A global allocator wrapper that fails loudly on allocation inside the callback checks it. No C equivalent without hooking `malloc` by hand — the strongest practical argument for the language. |
| Read files through a hand-written libsndfile FFI | About forty lines of `extern "C"`. The published binding crates stopped moving in 2021; at this size owning the declarations is sturdier and auditable, and the C library is healthy and packaged everywhere. `symphonia` was the alternative and cannot be used — no RF64 or Wave64. |
| Output format is `S24_LE`, and the ring matches it | The drivers declare formats statically, so this was read from kernel source rather than waiting for hardware: `WM8804_FORMATS` is `S16_LE / S20_3LE / S24_LE` and offers no `S32_LE`, so the intersection with `bcm2835-i2s` is `S16_LE / S24_LE`. `S24_LE` is right-aligned in a 32-bit word, so the ring holds `sf_readf_int`'s value shifted right 8 — done in the window thread, off the deadline. Still a pure shift, so still lossless. |
| GPIO 5 is the 44.1 kHz crystal, GPIO 6 the 48 kHz one | Named in the overlay as `clock44-gpio` and `clock48-gpio`, switched by the machine driver on every rate change. Reassigning them does not merely break oscillator select: the driver falls back to a 27 MHz sysclk, which is the WM8804's PLL reference, so audio keeps playing through the high-jitter path the build exists to avoid. |
| `sf_readf_int` supplies the ring | Its documented convention puts the source's most significant bit at the destination's most significant bit, so int16 arrives shifted left 16 and int24 left 8. The byte swap and the 24-bit unpack are therefore not our code at all; the only thing added is the right-8 shift into `S24_LE`. |
| Format scope is exactly the DDC's limit | 44.1 / 88.2 / 176.4 and 48 / 96 / 192 kHz; **int16 and int24 only**. 32-bit (int and float) and 8-bit are out, DSD is out, compressed is out. Nothing separate to remember: if the Digi2 Pro can send it, the deck plays it. |
| Every supported conversion is a pure shift | `sf_readf_int` returns int16 as `value << 16` and int24 as `value << 8`; the ring holds those shifted right 8 to match `S24_LE`. Nothing is rounded, scaled or truncated anywhere, so v1's "unconditionally bit-perfect" holds with no exceptions. Excluding 32-bit is what buys this — float32 cannot be converted without a clipping or scaling decision, and scaling would be a gain stage. |
| The window is sized in bytes, not seconds | `min(60 s, N MiB)`. Keeps RAM constant across all six rates, and lets an unexpected hi-res file play with a shorter window instead of failing. N is not yet chosen, and it is **not only a jog-feel parameter**: the forward half is also how long playback survives a knocked connector, which is a real risk in a venue. At 64 MiB that is 60 s at 48/24 but 21 s at 192/24, so the grace period varies by rate — worth choosing deliberately rather than from feel alone. |
| **exFAT and HFS+, both from v1** | The criterion is a filesystem natively read-write on macOS **and** with a mature in-tree Linux driver. Both qualify. HFS+ ("Mac OS Extended, Journaled") is in-tree and `S: Maintained` with three maintainers and its own git tree, so the reverse-engineering objection does not apply to it. Supporting both costs nothing: mounting is a udev concern at a fixed path, so the application never learns which one it got. FAT32 is out on the 4 GiB file limit, which bites a 192/24 track at 31 minutes. ext4 is out because the preparing machine cannot write it cleanly. |
| Journaled HFS+ mounting read-only is a feature here, not a limitation | The driver forces `SB_RDONLY` on a journaled volume and tells you to use `force` at your own risk. That is exactly the policy already chosen for removable media, so the filesystem's restriction and the design requirement are the same thing. Never pass `force`. |
| Do **not** pass `nodecompose` on an HFS+ mount | HFS+ stores filenames decomposed, but `hfsplus_uni2asc` composes them on the way out by default, via `hfsplus_compose_table`. Leave that alone and Japanese filenames arrive precomposed, so the application needs no normalisation pass and the per-line character budget in open question 1 means what it says. Pass `nodecompose` and every dakuten becomes a second code point. |
| APFS is the one still requiring out-of-tree code | Not in the kernel — `fs/` carries exfat, hfsplus, ntfs3 and udf but no apfs — so it would mean `apfs-fuse` (read-only, FUSE) or an out-of-tree module rebuilt per kernel update, both reverse-engineered, neither able to read FileVault volumes. Moot for now: the drive in question is HFS+, not APFS. If it ever matters, the fixed-path udev design means a driver plus one udev branch and **zero lines of Rust**. |
| Treat the mount as fallible, but do not design around it | Removal is the dominant case and normally the only one: leave it plugged in and it stays. The two non-removal paths are USB re-enumeration after a power dip — which is exactly the bus-powered-large-drive risk noted above — and a bumped connector, USB having no latch, in a venue where things get knocked —
which is confirmed as the use case, not assumed. Even so it does not change the
code: handling it is free. Neither is worth engineering for, because handling it is free: `read_dir` returns a `Result`, so the requirement amounts to "do not `unwrap()`", and the only real decision is which UI state to show. The audio side already survives it by design. |
| The UI distinguishes "no stick" from "stick I cannot read" | Showing only `No USB` for an unreadable filesystem makes a formatting mismatch look like broken hardware. Same principle as refusing a file on highlight and saying which of the four reasons applies: say *why*, not just *that*. |
| A udev rule calls `systemd-mount`, at a **fixed path** | Nothing mounts removable media on a headless box — the kernel creates the block device and stops. `systemd-mount` from `RUN{program}+=` is the documented pattern, with an example in its own man page; it creates a transient `.mount` unit, so teardown on removal is automatic. Calling `mount` directly from `RUN=` would leave the mount unowned. `SYSTEMD_MOUNT_WHERE=` and `SYSTEMD_MOUNT_OPTIONS=` are udev properties made for exactly this, so the fixed path and the per-filesystem options need nothing hand-rolled. It also keeps the mount privilege out of the audio process, which otherwise needs only `rtprio` and `memlock` and no root at all. |
| Match on `ID_FS_TYPE`, not "the first block device" | Correcting an earlier version of this row. A Mac drive initialised with a GUID partition map carries a vfat EFI System Partition, so taking `sda1` would have mounted that instead of the music; a flash stick formatted exFAT is often a single MBR partition where it would not. Matching only `exfat` and `hfsplus` does the partition selection as a side effect — and it is needed anyway, because the two filesystems take different option names so `-t auto` cannot supply them. |
| The fixed mount point is what enforces "one stick" | A second volume finds the path occupied and simply does not mount, so "the first one only" falls out of path uniqueness instead of needing selection logic. The application watches one known path and never discovers where anything landed. |
| `--automount=no`, explicitly | Automount is implied for removable devices, and it makes the mount point exist whether or not media is present — which destroys the simplest presence test. With it off the path appears only when something is mounted, so "No USB" versus the browser is one `stat`. |
| Ownership comes from mount options on both filesystems | exFAT has no POSIX ownership at all; HFS+ has it but carries the *preparing Mac's* uids. Either way `uid=`/`gid=` decide who can read, and getting them wrong means the mount succeeds while the application cannot read a thing. Numeric uid only — both drivers use `fsparam_uid` and the kernel resolves no names. Removable media, so `ro,nosuid,nodev,noexec` too. Charset is `iocharset=utf8` on exFAT (`utf8` is deprecated) and `nls=utf8` on HFS+; exFAT also has `fmask`/`dmask`, HFS+ only `umask`. Exact option lists are in `implementation.md`, read from the drivers. |
| The medium is solid-state — SSD or flash, never a spinning disk | Recorded so the concern is not re-derived. A spinning drive would have brought spin-up current spikes, seconds of latency on first access after mount, spin-down to disable, and seeks scattered by `readdir` order. None of it applies. A bus-powered SATA SSD in an enclosure draws a few hundred mA under load and a flash stick less, both comfortably inside the Pi's port budget, so no powered hub is needed. Sustained read is a non-question either way: the worst case the window thread asks for is 1.5 MB/s, at 192/24. |
| The cue key's volume UUID comes from blkid, not from the mount | Neither filesystem exposes its serial through a file API — exFAT keeps it in the boot sector, HFS+ in the volume header. Read it from `/dev/disk/by-uuid/` or the udev environment. The two formats differ (`XXXX-XXXX` versus a 16-hex UUID), so store it as an opaque string rather than parsing it. |
| One stick, the first one, mounted read-only | Read-only means a stick pulled mid-set cannot corrupt the filesystem, and a read-only mapping has no dirty pages to write back. |
| Header reads are driven by renders, not by encoder events | The browser must open the highlighted file to show its length, rate and depth, and to apply the four rejections before PLAY. On solid-state media that is **estimated** at 1-3 ms, and a cheap flash controller with poor random read could be several times that. Because redraws are already coalesced to 30-50 ms, only the positions actually *rendered* need headers — the intermediate positions of a fast spin are skipped outright — so the redraw budget bounds the read load and no second timer is needed. |
| Cache header reads by path, and read the whole visible page | The cache is what makes it cheap rather than the debounce: scrolling one row leaves 15 of 16 visible rows already read, so the marginal cost is one open. A folder change or page jump costs a full page, 16-32 ms, which fits inside one redraw interval. Reading the whole page rather than the highlighted row alone is then affordable, and it is what lets unplayable files be marked at a glance instead of one at a time. |
| Cue points live on the Pi, not the stick | Keyed by volume UUID plus relative path. The stick is content; the Pi owns state it created. Consequence: cues would not travel between two decks, if a second one is ever built. |
| FF / REW are a silent seek in v1 | Audible scan needs the resampler, which would give v1 a second mode and break "unconditionally bit-perfect". Position advances while held, the display follows, audio resumes on release. In v2 it becomes `r = 4` on the existing rate variable — no new mechanism. |

## Reversed during design

Recorded because the superseded reasoning is plausible enough to be re-derived by
accident.

| Was | Now | Why it flipped |
|---|---|---|
| Unify the library at 96 kHz to keep the resample ratio near 1 | Native rate per track | Matching output rate to source achieves the same ratio *and* costs less disk and CPU *and* permits bit-perfect. Upsampling 44.1 to 96 gains nothing. |
| libsamplerate SINC_BEST 145 dB / MEDIUM 121 dB | All three sinc converters are **97 dB**; they differ only in bandwidth | Checked the shipped docs. 97 dB is ~16 bits, which would cap the system at 16-bit whenever pitch is off centre. Switched to libsoxr. |
| Add an RP2040 to decode the jog encoder | Not needed | That was sized for 600 PPR (~12,000 IRQ/s while scratching). At the agreed coarse resolution it is ~800 IRQ/s, which the Pi absorbs. |
| Keep the library on the SD card, not USB | USB is fine — and later, USB is the *only* place it lives (see below) | That was because a USB DDC shared the Pi 3B+'s single USB 2.0 bus with the audio. The DDC is I2S; only Ethernet is left on that bus — Gigabit on the 3B+, but still behind USB 2.0 and still idle during playback. Superseded a second time by the removable-stick row below: read "USB SSD" there as "USB stick". |
| Mitigate OLED noise (series resistors, minimal refresh) | Not needed for noise | The IsolatorPi III puts the display on the near side of the isolation gap. Refresh discipline is still worth it for burn-in and I2C bus time. |
| Power quality is low priority | It matters | That rested on an async USB DDC owning the clock. With an I2S HAT inches from the Pi sharing power and clock, it does not hold. |
| `rotary-encoder,pin_a=5,pin_b=6` | Any other pins | GPIO 5/6 are oscillator-select lines. Originally attributed to the isolator; the manual shows J6 pins 29/31 are "isolated GPIO5 and GPIO6" — Pi pins *routed through* to the audio card. Removing the isolator does not free them, which the first phrasing invited. |
| Raw PCM removes the 4 GB container limit | It removes it only where import expands the data | Correct objection during design: the source WAV/AIFF is capped by the same 32-bit chunk size, so playable length does not grow. The limit only binds when normalisation inflates a legal source past it — which is also an argument against upsampling. |
| USB gadget mode to reach the Pi over one cable | Impossible on a 3B+ | Its micro-USB port is power only — the product brief lists it under Input Power; OTG exists on Zero-class boards and the Pi 4's USB-C. |
| Library resident on the Pi's USB SSD | The removable USB stick **is** the library | DJ practice, not streamer practice: the stick is prepared elsewhere and swapped often, and a CDJ has no internal storage at all. The Pi then needs only its SD card, for the OS and its own state. The original framing was a music streamer that played DJ-style. |
| "SSH is the only way in" means content arrives over the network | It means admin access only | Operation is fully network-independent. Ethernet is for maintenance and development, and the cable can be unplugged during a set — which removes an interrupt source, extending the same logic that turns the radios off. The line in `hardware.md` is about not locking yourself out, not about the content path. |
| Normalise to raw PCM at import, on a machine that is not the Pi | No import step; sources play as they are | WAV/AIFF-only means there is nothing expensive to move off-Pi — no decoder ever runs either way. Byte swap (`rev16`/`rev32`) and 24-to-32-bit unpack are a few instructions per sample, and they move to the window thread, which has no deadline. The "one idea" was sized for decoders this project had already excluded. |
| libsndfile must not appear in the playback path | libsndfile runs in the window thread | The ban's stated reason — it buffers, allocates and locks — only binds a thread with a deadline. In the window thread it brings big-endian AIFF, `sowt`, 24-bit unpacking, the 80-bit COMM sample rate **and RF64/Wave64** for free. RF64 matters: plain WAV caps a 192/24 track at 31 minutes. |
| mmap the PCM file and mlock a window around the playhead | libsndfile reads into a locked RAM ring held in the output's own `S24_LE` layout | With conversion in the path a RAM buffer is needed anyway, so mmap's "not more code, no ring buffer" advantage is gone. Dropping mmap also removes SIGBUS when a stick is pulled mid-playback, and removes the 32-bit address-space ceiling on multi-GB files. Jog still works: the ring is RAM, so reads inside it are free in either direction. |
| Metadata in SQLite or a sidecar beside the PCM | Neither; the folder tree is the index | With no import step, nothing writes a database. Headers are read lazily for the highlighted row — which is also the mechanism that catches every unplayable file before PLAY is pressed. |
| Restrict the rate budget to 44.1 and 48 kHz only | All six family rates | Proposed during design and withdrawn the same day. Sizing the window in bytes already fixes RAM at a constant regardless of rate, and v1 has no resampler, so restricting v1 bought nothing at all. v2's cost is handled by unity as the fallback, not by narrowing v1. |
| The 3B+'s 1.4 GHz gives v2 more headroom than the 3B's 1.2 GHz | Size against 1.2 GHz anyway | The 3B+ has a *soft* temperature limit that drops the clock from 1.4 to 1.2 GHz at 60 C by default (raisable only to 70, "might cause instability"). A deck runs a continuous load in a box, so 1.2 GHz is the steady state. The 3B+ buys thermal mass and Gigabit Ethernet, not resampler headroom. Corollary: benchmark soaked, not cold. |

## Open

**1. Display.** Japanese filenames make 128x64 marginal — 12x12 is the practical
floor for kanji, giving 10 characters per line at 128 px.

| Option | Chars/line | Lines | Glyph size | Note |
|---|---|---|---|---|
| SSD1309 2.42" + Misaki 8x8 | 16 | 7 | 3.4 mm | Dense kanji blur; kana fine |
| SSD1322 256x64 + 12x12 | 21 | 4 | 3.4 mm | Four visible entries is thin for browsing |
| ILI9341 2.8" TFT + 16x16 | 20 | 13 | 2.9 mm | Best usability, cheapest, no burn-in — but backlit, not OLED |

Fonts (all free, BDF): Misaki 8x8, Shinonome 12/16, k8x12 (8 px halfwidth /
12 px fullwidth, a good middle for mixed filenames). Rust reads BDF via the `bdf`
family of crates, or bake the glyphs to a bitmap atlas off the deck — which is
faster on an A53 and fits the project's own habit of moving work off the deadline.

Suggested: prototype the UI on a cheap 0.96 in panel — unreadable, but it settles
what fits in how many pixels — then choose. `embedded-graphics` keeps the choice
reversible: drivers exist for every controller listed above (ssd1306, ssd1309,
ssd1322 including a 256x64 variant, ssd1327, ili9341, st7789), and
`linux-embedded-hal` puts them on the Pi's `/dev/i2c` and `/dev/spidev`.

**2. libsoxr on A53** — unmeasured. Benchmark precision x output rate x pitch
range for realtime ratio *and* worst-case block time.

It no longer decides whether the board is viable. v1 has no resampler, so the 3B+
plays all six rates regardless; and in v2 a rate the resampler cannot sustain
falls back to **unity**, which is already a designed path and is bit-perfect. So
what this measures is **which rates get pitch and jog**, not whether a Pi 4 is
needed.

Still run it **before** the enclosure fixes the board, and run it **thermally
soaked**: the 3B+ throttles 1.4 -> 1.2 GHz at 60 C, so a cold run measures a clock
a set will not hold. Set the pitch-range axis from the material actually played —
sustained long-form work sits near `r = 1.0`, which is the cheap end, and does not
need the club-width range as its worst case.

**3. ENTER as its own button?** Cheap encoder push switches bounce and wear, and
ENTER is the most-used control. Pin budget allows a dedicated button.

**4. Two hardware facts still needing the physical boards.** Recorded in
`docs/hardware.md` under Sources - Unverified.

*The exact J12/J13 master-mode jumper values.* The manual's table is pictorial, its
worked examples and its board silkscreen appear to disagree, and a third-party
mirror reverses it. Silent when wrong, and a jumper in the wrong orientation can
destroy the isolator.

*What the Digi2 Pro's "isolation ground jumper" does.* Undocumented, but the
datasheet's mention of an output isolation transformer narrows it to bonding or
floating the output ground — which decides whether the clean side is referenced
through the coax shield, and therefore feeds straight into question 6. This is the
one to chase first.

A third item, whether the Digi2 Pro has a pass-through GPIO header, is close to
answered: the datasheet enumerates its connectors and none is a pass-through. Treat
it as a terminating HAT, which means bring-up step 2 needs a breakout.

**5. Enclosure vs. the 60 C soft limit.** The official guidance is that a case
"should not be covered", and a sealed DJ enclosure covers it. A fan is an acoustic
noise source in a listening context and an electrical one next to the audio
boards; passive options are a vented enclosure, a heatsink, or accepting 1.2 GHz
as permanent (which the sizing already does). Depends on outcome 2.

**6. Power topology, beyond the clean-side budget.** Deliberately deferred, not
overlooked. `hardware.md` budgets the clean side and records J1's 3.3-5 V range,
the 4.8 V floor and the GPIO-header input, but three things are unworked: the Pi
side's own current budget, whether the clean supply's secondary must float (its
ground reference arrives through the S/PDIF shield from the DAC, so an
earth-bonded output could form a loop), and power-on order (the isolator driving
SCK/LRCK into an unpowered Pi is the direction to worry about). None of it binds
until bring-up step 3, which is when the isolator goes in.

## Phases

**v1** — browse and play. Unconditionally bit-perfect: with no pitch and no jog
there is no other mode, so no resampler and no unity button. Encoder, three
buttons, display.

**v2** — pitch fader (needs an SPI ADC; the Pi has none), jog wheel, libsoxr, the
unity button. The v1 read path, control-thread shape and rate variable are all
built to accept this without rework.

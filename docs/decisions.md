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
| The window's two halves are relative to the **direction of travel** | Not to increasing frame number. An append-only ring accumulates for free only on the side the playhead has *passed*, so the useful half depends on which way it is moving — and a forward-only refill served a descending playhead **0 of 12 periods**, measured. Direction is inferred from successive playhead values, so nothing new is plumbed, and an explicit seek clears it because a cue jump backwards is a discontinuity rather than motion. Residual cost, stated rather than hidden: one missed period per relocation, because relocating discards the window and a descending playhead's next input is read *last*. Reverse playback is served, not gapless; making it gapless means a ring that can write below `start`, which is not built. |
| Wireless off, Ethernet only | Fewer interrupts, and the 3B+ radio is dual-band, so this removes a 5 GHz transmitter as well as the 2.4 GHz one. Confirm wired access *before* disabling. |
| Buttons and encoders on GPIO, kernel-decoded | No MIDI jitter, no USB polling interval. `gpio-key` / `rotary-encoder` overlays, never userspace polling. |
| No LEDs | Keeps the GPIO wiring simple. Not a noise decision — a statically driven LED is DC and quieter than the display. The display shows state instead, and can show *why*, not just *that*. |
| **Everything in Rust, one process** | v1 would be comfortable in Python but v2 would not, so the engine was never going to be Python. The *split* then collapsed: the browser needs libsndfile too (to read the highlighted row's header), so a language boundary would mean binding it twice or asking the engine over IPC. Rust also has what kept the UI in Python — `embedded-graphics` gives luma's device-swap reversibility, which C has no equivalent for. |
| The realtime setup is applied **and read back**, in `src/rt.rs` | `mlockall`, `SCHED_FIFO` and core pinning are what make the callback's "cannot fault" true — it is a property of the process, not of the code, so none of `tests/callback_rules.rs` reaches it. All three fail silently: `ENOMEM`, `EPERM`, `EINVAL`, and audio keeps coming out in every case. So each is verified against `sched_getscheduler`, `sched_getparam`, `VmLck` and `sched_getaffinity`, and two are answered up front from `getrlimit` so a failure prints the missing `limits.conf` line instead of an errno. The pre-fault has nothing to ask, so it measures its own reach and `apply` refuses a short one — the first version looped over one array and touched one page, which reads as satisfied and prefaults nothing. `libc` rather than `thread-priority`: two of the three calls are not in that crate and the read-backs need the raw ones anyway. |
| Enforce the callback rules with `assert_no_alloc` | `CLAUDE.md` requires the discipline from the first commit, which is intent unless something checks it. A global allocator wrapper that fails loudly on allocation inside the callback checks it. No C equivalent without hooking `malloc` by hand — the strongest practical argument for the language. **Scope correction, measured:** it wraps Rust's `GlobalAlloc`, so a C library calling glibc directly is invisible to it — libsoxr imports `malloc@GLIBC_2.17` and friends. That costs nothing for libsndfile, which runs in the window thread, and everything for libsoxr, which is v2's callback. The decision stands; the claim that it *enforces* the invariant holds for v1 and not for v2. |
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
| What the DAC accepts is out of scope — if it does not play, it does not play | S/PDIF is unidirectional, so a DAC that cannot lock goes silent and nothing comes back; the software cannot detect it, ever. Accepted rather than engineered around. The display already shows the rate in use, which is the whole diagnosis, and an unknown downstream is handled by not carrying rates you cannot guarantee. A configurable ceiling was proposed twice and dropped twice: the deck cannot tell which DAC is attached, so it would be a claim, and a stale claim sounds certain while being wrong. |
| The UI distinguishes "no stick" from "stick I cannot read" | Showing only `No USB` for an unreadable filesystem makes a formatting mismatch look like broken hardware. Same principle as refusing a file on highlight and saying which of the four reasons applies: say *why*, not just *that*. |
| A udev rule calls `systemd-mount`, at a **fixed path** | Nothing mounts removable media on a headless box — the kernel creates the block device and stops. `systemd-mount` from `RUN{program}+=` is the documented pattern, with an example in its own man page; it creates a transient `.mount` unit, so teardown on removal is automatic. Calling `mount` directly from `RUN=` would leave the mount unowned. `SYSTEMD_MOUNT_WHERE=` and `SYSTEMD_MOUNT_OPTIONS=` are udev properties made for exactly this, so the fixed path and the per-filesystem options need nothing hand-rolled. It also keeps the mount privilege out of the audio process, which otherwise needs only `rtprio` and `memlock` and no root at all. |
| Match on `ID_FS_TYPE`, not "the first block device" | Correcting an earlier version of this row. A Mac drive initialised with a GUID partition map carries a vfat EFI System Partition, so taking `sda1` would have mounted that instead of the music; a flash stick formatted exFAT is often a single MBR partition where it would not. Matching only `exfat` and `hfsplus` does the partition selection as a side effect — and it is needed anyway, because the two filesystems take different option names so `-t auto` cannot supply them. |
| The fixed mount point is what enforces "one stick" | A second volume finds the path occupied and simply does not mount, so "the first one only" falls out of path uniqueness instead of needing selection logic. The application watches one known path and never discovers where anything landed. |
| `--automount=no`, explicitly | Automount is implied for removable devices, and it makes the mount point exist whether or not media is present — which destroys the simplest presence test. With it off the path appears only when something is mounted. **But "one `stat`" was too strong, and this row used to say it.** See the row below. |
| Presence is `st_dev` against the parent's, **not existence** | Correcting the row above, found while building `src/media.rs`. "The path appears only when something is mounted" is systemd's behaviour; the conclusion that an existence test suffices additionally depends on systemd *removing* the directory on unmount — and a failed unit, a hard removal, a crash or anyone's stray `mkdir` leaves it behind. An existence test then reports a stick that is not there and the browser opens an empty folder, which is a **third** way to confuse "no stick" with "stick I cannot read" in a design whose stated principle is telling those apart. Measured in a container: tmpfs mounted at the path gives `st_dev` 79 against the parent's 76; unmounted with the directory left behind, both read 76. One extra `stat` removes the assumption instead of relying on it. Two limits of the technique, neither reachable here — a filesystem root is its own parent, and a bind mount of the same filesystem shares `st_dev`. |
| Ownership comes from mount options on both filesystems | exFAT has no POSIX ownership at all; HFS+ has it but carries the *preparing Mac's* uids. Either way `uid=`/`gid=` decide who can read, and getting them wrong means the mount succeeds while the application cannot read a thing. Numeric uid only — both drivers use `fsparam_uid` and the kernel resolves no names. Removable media, so `ro,nosuid,nodev,noexec` too. Charset is `iocharset=utf8` on exFAT (`utf8` is deprecated) and `nls=utf8` on HFS+; exFAT also has `fmask`/`dmask`, HFS+ only `umask`. Exact option lists are in `implementation.md`, read from the drivers. |
| The medium is solid-state — SSD or flash, never a spinning disk | Recorded so the concern is not re-derived. A spinning drive would have brought spin-up current spikes, seconds of latency on first access after mount, spin-down to disable, and seeks scattered by `readdir` order. None of it applies. A bus-powered SATA SSD in an enclosure draws a few hundred mA under load and a flash stick less, both comfortably inside the Pi's port budget, so no powered hub is needed. Sustained read is a non-question either way: the worst case the window thread asks for is 1.5 MB/s, at 192/24. |
| The cue key's volume UUID comes from blkid, not from the mount | Neither filesystem exposes its serial through a file API — exFAT keeps it in the boot sector, HFS+ in the volume header. Read it from `/dev/disk/by-uuid/` or the udev environment. The two formats differ (`XXXX-XXXX` versus a 16-hex UUID), so store it as an opaque string rather than parsing it. **Implemented with no subprocess:** `src/media.rs` walks the symlink farm and matches on **device number** — `st_dev` of a file on a block-backed filesystem is the block device's `st_rdev` — so no path or partition name is parsed or guessed. Verified against a real loop-mounted volume in a container: the value returned matches `blkid -s UUID` exactly. |
| **No volume UUID is a browsable state, not a failure** | The stick browses and plays; what it loses is the cue store's key, so cues cannot be persisted for it. Encoded in the type (`Browsable { uuid: Option<String> }`) rather than left to a caller to remember, and the state says so in words, because a silently non-persisting cue button is worse than one that explains itself. Reachable in practice whenever udev has not published an entry — a filesystem with no UUID at all, or a volume that is not block-backed. |
| One stick, the first one, mounted read-only | Read-only means a stick pulled mid-set cannot corrupt the filesystem, and a read-only mapping has no dirty pages to write back. |
| Dotfiles are hidden, and on this medium that is **not cosmetic** | Found while building the browser, not designed in. The stick is prepared on a Mac, so every folder carries `.DS_Store`, and an HFS+ volume additionally carries `._name` AppleDouble twins plus `.Spotlight-V100`, `.fseventsd` and `.Trashes`. `._piece.wav` is the one that matters: libsndfile cannot open it, so without the rule it would sit **directly beside** `piece.wav` and read as a broken duplicate of it — the worst possible neighbour for a file that does play. One rule removes all of them. Note this is the *only* filter: a file the deck cannot play is still shown, marked, with the reason, because hiding refusals would make a folder full of FLAC look empty. |
| Folders first, then files; each group in **natural** order | Not specified anywhere in these documents, so it was chosen while writing `src/browser.rs`. `readdir` order on both filesystems is whatever the directory structure yields — neither stable nor meaningful — so *some* order had to be imposed. Folders first is the file-manager convention and it matters more here than usual: with **two** browsable rows on the smallest candidate panel, having the navigable rows collected at the top is the difference between seeing a folder and scrolling to find it. Numeric-aware because prepared music is commonly numbered and plain lexicographic order puts `10` before `2`; it costs nothing on unnumbered names, so **no assumption about the material is involved** — it is safe either way. Digit runs are compared as strings after stripping leading zeros rather than parsed, so forty digits in a filename cannot overflow anything, and there is a final tie-break on the raw bytes so the order is **total** — without it `A` and `a` compare equal and a reload could reshuffle rows under the selection. |
| The selection does not wrap at either end | A detented encoder gives no feedback that a list ended. On a two-row panel, wrapping from the end of a long folder to its start is indistinguishable from a mis-scroll; stopping is legible, and the top and bottom of a list are places you can feel. A CDJ's list does wrap, so this is a deliberate departure and the reason is the row count. |
| Header reads are driven by renders, not by encoder events | The browser must open the highlighted file to show its length, rate and depth, and to apply the four rejections before PLAY. On solid-state media that is **estimated** at 1-3 ms, and a cheap flash controller with poor random read could be several times that. Because redraws are already coalesced to 30-50 ms, only the positions actually *rendered* need headers — the intermediate positions of a fast spin are skipped outright — so the redraw budget bounds the read load and no second timer is needed. |
| Cache header reads by path, and read the whole visible page | The cache is what makes it cheap rather than the debounce: scrolling one row leaves all but one visible row already read, so the marginal cost is one open. A folder change or page jump costs a full page. **The figures here were sized against sixteen visible rows, which is not any candidate in open question 1** — those give four to thirteen — so a full page is 4-13 opens, an estimated 4-39 ms, and it fits inside one redraw interval at the low end of that. The conclusion survives the correction; the arithmetic did not, and a wrong number supporting a right conclusion is exactly what gets re-derived later. Reading the whole page rather than the highlighted row alone is then affordable, and it is what lets unplayable files be marked at a glance instead of one at a time. |
| **Three input disciplines, not one** | Built as `src/input.rs`. BACK, PLAY/PAUSE and ENTER fire on the key-**down** — no second meaning, and these are where latency is felt. CUE emits **press and release** and the transport picks from its own state, because the Cue Point Sampler "continues while the button is held in" and so cannot wait for a threshold; `cue_down`/`cue_up` were already exactly this pair. Only FF and REW get tap-versus-hold, because only they carry two meanings. **A uniform rule would be simpler and wrong: PLAY held slightly long would emit a hold and never a tap, so the deck would not start.** |
| The hold threshold is timed from a **monotonic read**, not the event's timestamp | Every kernel input event carries one, and using it is the obvious thing. Kernel input timestamps are **`CLOCK_REALTIME` by default**, so an NTP step — plausible while the Ethernet cable is in for maintenance — would turn a tap into a forty-minute hold. `EVIOCSCLOCKID` would switch the device to `CLOCK_MONOTONIC`, but a monotonic reading taken when the event is *read* is accurate to microseconds against a 300 ms threshold, so the ioctl and its failure mode buy nothing. The decoder takes the time as an argument, which is also what makes tap-versus-hold testable with no device and no sleeping. |
| The encoder's axis type is **not** assumed | The `rotary-encoder` overlay reports a relative or an absolute axis depending on a parameter, and `hardware.md` says to check the overlay's parameters on the actual image rather than trusting a spelling. Both are handled, so the code does not depend on a line of `config.txt` nobody has run yet. The absolute flavour needs one extra rule: the **first** reading establishes a baseline and emits nothing, or an encoder that starts anywhere but zero scrolls the browser that far on its first event. |
| Cue points live on the Pi, not the stick | Keyed by volume UUID plus relative path. The stick is content; the Pi owns state it created. Consequence: cues would not travel between two decks, if a second one is ever built. |
| The cue file is hand-written, line-based, and **escapes the path** | Built as `src/cue.rs`. Hand-written because the whole need is a map from a path to a `u64`, which is smaller than the crate that would read it, and because a format you can `cat` is worth having on an appliance you SSH into. **What decides the format is that a filename can contain a newline**: HFS+ permits almost any Unicode except `:` and `/`, U+000A included, and it was verified as a real filename on both a Mac and a Linux container. A naive `frame<TAB>path` line is then not untidy but *wrong* — the entry splits and the second half parses as garbage or as another entry. exFAT's spec forbids control characters so it cannot happen there, but the drive in question is HFS+. Multi-byte UTF-8 passes through unescaped, so Japanese names stay legible. |
| The cue key is **raw bytes**, not a `String` | A filename is bytes. A lossy conversion maps every invalid sequence to the same replacement character, so two different broken names would collide on one cue — silently, and only on the medium that produced them. Tested with two distinct invalid-UTF-8 names. |
| Setting a cue **writes through, atomically** | A deck gets switched off at the wall, so a store that flushed only on shutdown would routinely lose the last thing set; the write is a few hundred bytes off the audio thread and an SD card does not care. Atomic — temporary beside it, then `rename` — because a power cut mid-write would otherwise lose *every* cue for that stick rather than one. The file is sorted so re-saving is byte-identical and a diff shows what changed. |
| State lives in `$XDG_STATE_HOME/deck-pi`, one file per volume | State rather than config or cache: the deck created it and cannot regenerate it. Not `/var/lib`, because the audio process runs as a plain user — it needs only `rtprio` and `memlock` and no root. One file per volume UUID means dropping a stick you no longer use is deleting one file. A volume name that cannot be a filename is **refused, not sanitised**: mangling two volume names into one file would merge their cues, which is worse than not saving. |
| CUE follows the CDJ-350, and there is no separate STOP | Read out of the CDJ-350 operating instructions (Pioneer 389414-01U, p.18) rather than recalled — the 350 being the right reference for a five-button single player. Paused, CUE **sets** the point; playing, it **returns** there and pauses (Back Cue); held at the point, it **plays while held** (Cue Point Sampler). One cue point per track, setting a new one cancels the old, and setting it outputs no sound. The finding that matters: a CDJ has no STOP function, because returning to the cue point and pausing *is* stopping — so "CUE / STOP" is one function, not two, and the hold gesture is free for preview instead of being spent on a stop the transport already has. Costs no new mechanism: hold is `r = 1.0`, release is `r = 0` with the position snapped back. |
| Auto cue is not adopted | The CDJ-350 skips the silent lead-in on load and places the cue just before the sound starts, thresholds from -36 to -78 dB. **The rejection stands but not its original framing**, which was "sensible for club material, wrong here" — the deck plays club material too, so the split does nothing. It stands on the quieter case alone: one piece that opens below the threshold deliberately is enough to make an automatic decision wrong, and letting the deck decide where the music "really" begins is the kind of silent, well-meant alteration this project exists to avoid. A rule has to be right for everything on the stick, not for most of it. The cue starts at frame zero unless set. |
| A track that reaches its end stops | Nothing starts on its own. In a venue, a next track beginning while attention is elsewhere is worse than a silence, and PLAY is right there. The engine already reports `EndOfTrack` and decides nothing, so this is where the decision lands. Auto-advance is a small addition later if wanted. |
| Raspberry Pi OS Lite (64-bit) is the base | Four constraints already in these documents pin it down. The HiFiBerry overlay and `rpi-wm8804-soundcard.c` live in the **downstream** `raspberrypi/linux` tree, not mainline, so the OS must ship the rpi kernel and its overlays. The mount design commits to `systemd-mount` and its transient `.mount` unit — the thing that makes teardown on removal automatic — so systemd is not optional. `gpu_mem`, `dtoverlay`, `temp_soft_limit`, `core_freq_fixed` and `vcgencmd` are the Pi's own firmware path, and `config.txt` on the FAT partition is what makes a bad setting recoverable by reading the card on a Mac. And exfat and hfsplus must be in-tree, with `iocharset` and `nls`. 64-bit is not an argument either way — `architecture.md` records that OS bit width stopped being a design input once mmap was dropped — just the current default, and it makes cross-building on an arm64 Mac direct. |
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
| **The material is long-form** — sustained ambient and drone | The material is **various**: dance, ambient and other kinds | The sixth invented premise, and the widest-reaching — load-bearing in ten places across four files, and it had to be found by `grep` rather than by argument. It began as a true statement the user made about *part* of their material and hardened into a claim about all of it, which is harder to catch than an invention from nothing because the origin is a real quotation. What it actually changed, once the seventh premise below was also unwound: **the browsable-rows objection got worse**, because folders of many short tracks are scrolled more than folders of twenty long ones — so the row count matters most for the material that was missing. What it only reworded: cues are a mixing tool *and* an in-track navigator; FF/REW's two halves each earn their keep on different material; the OLED idle timer. What it did not touch: the auto-cue rejection, which stands on the quiet case alone, because a rule must hold for everything on the stick rather than for most of it. |
| **Dance material means beatmatching**, so the pitch range is club-width and losing pitch means the deck fails at its job | One deck, changing speed **like a record**. No second source to sync to. **±10% is enough** | The seventh, and mine — written *while* recording the sixth. The user said dance material exists; "therefore beatmatching, therefore club-width, therefore not graceful degradation" was all inference, and it had become the foundation of a sharpened open question within one message. The correction is structural rather than a matter of degree: with no second deck there is no drift to correct continuously. Losing pitch on an unsustainable rate costs the **speed control** for that track — real, more than a nicety, not a sync failure. And ±10% turns open question 2's pitch range from an axis into a constant. |
| `SOXR_VR` allocates in the audio callback, unboundedly while the ratio moves | It allocates **nothing** — the probe had misconfigured `soxr_create` | Retracted in full. Every measurement created the resampler as `soxr_create(1, 1, ...)` and then set ratios from 0.90 to 1.10; the create-time ratio is what sizes `SOXR_VR`'s buffers, so it grew into a range it had not been told about. Declaring the span gives **0 events, 0 reallocs, 0 bytes** over 1,161 s, with output identical at every ratio — so the zero is not a resampler doing nothing. What came out of the files: the event counts, the saturation table, the three usage regimes, the "unbounded while the ratio moves" scoping, the churn mechanism, the per-event costs, the duty cycle and the two over-budget periods. **Every figure was internally consistent and arithmetically correct, which is why verifying them could not catch it** — they were right about the wrong thing; only the API call was wrong, and it had been guessed because `soxr.h` documents variable-rate creation as "see example # 5". Two things survive: `assert_no_alloc` cannot see libsoxr at all, and **declaring the pitch range at `soxr_create` is now a requirement** in `implementation.md` — misconfigure it and the audio is correct, the allocation is unbounded, and nothing reports it. |
| The 3B+'s 1.4 GHz gives v2 more headroom than the 3B's 1.2 GHz | Size against 1.2 GHz anyway | The 3B+ has a *soft* temperature limit that drops the clock from 1.4 to 1.2 GHz at 60 C by default (raisable only to 70, "might cause instability"). A deck runs a continuous load in a box, so 1.2 GHz is the steady state. The 3B+ buys thermal mass and Gigabit Ethernet, not resampler headroom. Corollary: benchmark soaked, not cold. |

### Operating systems considered and dropped

Recorded because "surely something leaner" is exactly the kind of thing that gets
re-derived.

| | Why not |
|---|---|
| **DietPi** | The near miss — same Debian + rpi kernel + systemd base, and leaner, which is genuinely attractive for an appliance. It loses because it inserts its own configuration layer that owns `config.txt` and service enablement, and a second actor mutating a load-bearing file is a real cost in a project whose central fear is a setting that is silently wrong. Worth reconsidering at the stripped-image stage. |
| **Ubuntu Server for Pi** | systemd and a raspi kernel, but its own firmware and overlay packaging and `/boot/firmware` layout. Both HiFiBerry's and Ian Canada's instructions target Raspberry Pi OS. Friction with no benefit. |
| **Alpine** | No systemd by default, which breaks the documented mount design outright. |
| **Arch ARM, Fedora, openSUSE** | Mainline-kernel or thin on Pi 3. An appliance should be boring. |
| **moOde, Volumio, piCorePlayer** | Excluded *because* they already drive HiFiBerry and claim bit-perfect: they **are** the player. Basing on one means fighting MPD for exclusive `hw:` access. |
| **Buildroot, Yocto, NixOS** | Possible destination for a stripped appliance image, but after the measurements, not before — bring-up needs `alsacap`, `amixer`, `dtoverlay -h`, `vcgencmd`, `rfkill`, `blkid` and `findmnt`, and a minimal image strips all of them. The case is also weaker than it looks: what they mainly buy is kernel control, and `architecture.md` already says PREEMPT_RT is likely unnecessary. |

Three things the OS choice does **not** settle: whether the rootfs ends up
read-only, whether bring-up and production get separate images, and how the deck
starts at boot.

## Open

**Status lives in GitHub issues; the reasoning lives here.** Deliberately not both —
two copies of an analysis means one of them goes stale, and this file exists
precisely so a dropped line of reasoning is not re-derived by accident. An issue
carries the question, its dependencies and **what would close it**; the sections
below carry why it is hard.

| | Question | Issue |
|---|---|---|
| 1a | **v2 ADC: I2C or SPI** — decides the display *and* the button ceiling | [#1](https://github.com/tamatebox/deck-pi/issues/1) |
| 1b | Which display panel (blocked on 1a) | [#2](https://github.com/tamatebox/deck-pi/issues/2) |
| 2 | Benchmark libsoxr on the A53, thermally soaked | [#3](https://github.com/tamatebox/deck-pi/issues/3) |
| 3 | ENTER: the encoder's push, or its own button? | [#4](https://github.com/tamatebox/deck-pi/issues/4) |
| 4 | What the Digi2 Pro's `JP1` does | [#5](https://github.com/tamatebox/deck-pi/issues/5) |
| 5 | Enclosure vs the 60 C soft limit | [#6](https://github.com/tamatebox/deck-pi/issues/6) |
| 6 | Power topology: grounding, Pi-side budget | [#7](https://github.com/tamatebox/deck-pi/issues/7) |
| 7 | How the deck starts at boot | [#8](https://github.com/tamatebox/deck-pi/issues/8) |

**Six more were open and not on this list**, found by grepping for what the
documents call unsettled rather than by reading the numbered section. One of them
existed only as a comment in `tools/panel-compare/Cargo.toml`.

| Question | Issue | Where it was hiding |
|---|---|---|
| Choose `N`, the window size in bytes | [#9](https://github.com/tamatebox/deck-pi/issues/9) | "the value is not yet chosen", twice, in prose |
| Font licensing if the deck ships u8g2's Japanese sets | [#10](https://github.com/tamatebox/deck-pi/issues/10) | a `Cargo.toml` comment |
| The mount point and uid are placeholders | [#11](https://github.com/tamatebox/deck-pi/issues/11) | "chosen here and not decided anywhere" |
| What an FF/REW tap does at a folder boundary | [#12](https://github.com/tamatebox/deck-pi/issues/12) | one `Open:` line in `hardware.md` |
| **What an FF/REW tap acts on at all** — and who owns "what is loaded" | [#14](https://github.com/tamatebox/deck-pi/issues/14) | nowhere; found by building `src/input.rs` |
| Does the Digi2 Pro have a pass-through header? | [#13](https://github.com/tamatebox/deck-pi/issues/13) | `hardware.md` § Unverified |
| Read-only rootfs; separate bring-up and production images | [#8](https://github.com/tamatebox/deck-pi/issues/8) | "three things the OS choice does not settle" |

**1. Display.** Japanese filenames make 128x64 marginal — 12x12 is the practical
floor for kanji, giving 10 characters per line at 128 px.

**The table used to omit the axis that decides it: the interface.** I2C costs no
pins, sharing the bus the WM8804 is already on; SPI costs the SPI0 block plus DC and
RESET. So a panel choice is also a pin-budget choice, and the ILI9341 looks like a
clean win on the old columns while being the most expensive in pins.

**The SPI pin cost is contingent, though**, and the two sections below are what
establish that — read them before treating the `5*` column as a cost.

**Rendered, not estimated.** The figures below are read off actual frames from
`tools/panel-compare`, which draws the same folder listing at each geometry with real
Japanese filenames. The `Rows` column is now **browsable rows** — what is left after
the path header and the transport status bar — which is the number that matters and
is not what an earlier version of this table reported.

| Option | Bus | Extra pins | Chars/line | Rows | Glyph | Note |
|---|---|---|---|---|---|---|
| SSD1309 2.42" + Misaki 8x8 | **I2C** | **0** | 15 | 5 | 3.4 mm | **Confirmed unusable by rendering it.** Kana are marginal and kanji are unrecognisable blobs — 雨音と遠雷 and 第三楽章 do not resolve. Big and wrong |
| SSD1309 2.42" + 12x12 | **I2C** | **0** | 9 | **2** | **5.2 mm** | Biggest glyphs by far — but **two** browsable rows, not the four this table used to claim |
| SSD1327 1.5" 128x128 + 12x12 | **I2C** | **0** | 9 | 7 | 2.5 mm | Correct shapes and enough rows; small. Ruled out on Gray4 and bus time below, not on legibility |
| SH1107 1.12" 128x128 + 12x12 | **I2C** | **0** | 9 | 7 | **1.9 mm** | The 128x128-mono "escape". Visibly too small; see below |
| SSD1322 256x64 + 12x12 | SPI | 5* | 20 | **2** | 3.4 mm | Wide and flat, and two rows for the same reason as the SSD1309 |
| ILI9341 2.8" TFT + 16x16 | SPI | 5* | 20 | **12** | 2.8 mm | Six times the rows of any I2C option, and the only one that can mark refusals in **colour** |

`5*` — SCLK, MOSI, CS, DC and RESET. **Zero net if the v2 ADC is I2C rather than
SPI**, because SPI0 is then unclaimed; the whole of it, and why it is the deciding
question, is two sections down.

**12x12 is the floor for kanji *shape*, independent of physical size**, and rendering
it settles the point: at 8x8 the kanji are not soft, they are wrong — 雨音と遠雷
resolves to blobs. Big and wrong is a different failure from small and right.

**And the row count was the real cost, understated until it was drawn.** The trade was
recorded here as the SSD1309's 5.1 mm over *four* browsable rows against the
SSD1327's 2.5 mm over ten. The actual frames give **two** and **seven**. Two rows is
not a folder browser; it is a two-line readout with a scrollbar's worth of context.
So the I2C branch is worse than this file claimed on the axis it claimed to win.

**And worse still than that.** This objection was first written against "a folder
holding twenty long pieces". The stick also carries dance — **folders of many short
tracks**, so more rows scrolled per set, not fewer. The row count matters *more* for
the material that was missing from the premise, which strengthens the twelve-row
option rather than softening it.

### That is not the deciding axis either. Two computed ones are.

The trade was recorded here as perceptual and therefore not computable, settleable
only by buying both. That was wrong: **the SSD1327 costs 8x the bus time and cannot
use what it buys.** Both halves are arithmetic.

**Bus time, at 400 kHz with 9 bits per byte on the wire:**

| | Bytes | Full frame | One 12 px row |
|---|---|---|---|
| SSD1309 128x64, 1 bpp | 1,024 | **23.0 ms** | 4.3 ms |
| SSD1327 128x128, 4 bpp | 8,192 | **184.3 ms** | 17.3 ms |

The 8x is 4x the depth times 2x the pixels. The 23.0 ms reproduces
`architecture.md`'s own "~26 ms over I2C at 400 kHz" — same model, the difference
being addressing overhead — which matters because **the entire refresh discipline
was sized against that number**: "redraw at most every 30-50 ms". A full SSD1327
frame is 4-6x over that budget, on a bus shared with the WM8804.

Per-row scrolling is fine on both — 17.3 ms is inside budget — so fast scrolling does
not break, and an earlier version of this section said it did. What the 184 ms costs
is a **one-shot hitch on discrete navigation**: a folder change, an ENTER, a page
jump. That is a UX judgement, not a hard failure, and it should not be dressed as
one. The sharpest form of it is a real collision, though: a track load at a new rate
is *simultaneously* a full redraw and the moment the driver needs the bus to
reconfigure the WM8804.

**Read that as contention, not a lockout.** I2C arbitration is per transfer and no
driver pushes 8,192 bytes as one transaction — panel drivers write page by page and
the kernel's I2C core interleaves transfers from different clients. So the two
operations stretch each other; there is no 184 ms window in which the codec cannot be
reached. Phrased as a lockout, someone would later try to reproduce a 184 ms stall
and fail to find one.

**The greyscale is unusable with the planned fonts, which is what makes the 8x
indefensible rather than merely expensive.** SSD1327 is Gray4. The font plan below is
1-bit BDF glyphs or a baked atlas, and a 1-bit glyph rendered into Gray4 stores only
0 and 15 — four bits per pixel holding one bit of information, with no antialiasing
anywhere. The eight-fold cost buys a capability the design cannot use. It would pay
off only with a vector or antialiased renderer, which is not the plan and would spend
A53 cycles v2 wants. Nor is per-pixel grey needed for the OLED idle dim: that is a
global contrast register, which the SSD1309 has too.

**There is no clock lever, and this is now sourced rather than assumed.** An earlier
version here said the Pi could run I2C at 1 MHz, putting a full SSD1327 frame at
~74 ms and still over budget. The truer statement is that **1 MHz is not available on
this bus at all**: the WM8804 datasheet (v4.5, Table 5) caps SCLK at **400 kHz**, with
`tSCY` minimum 2500 ns — exactly 1/400 kHz, so the table is self-consistent. Clocking
past it puts the codec out of spec.

`hardware.md` already has the observation this rests on, next door to the pull-up
question: the isolator splits I2C into two electrically separate segments, but it is
"one logical bus from the Pi's controller". The clock follows the same logic — one
controller, one rate, so the display segment cannot be clocked faster than the codec
segment tolerates. 400 kHz is the ceiling, full stop.

**Driver crate health points the same way**, as a soft third factor: `ssd1327` is at
0.1.0 with ~1,700 downloads and untouched since 2020-11 — staler than
`assert_no_alloc`, which the dependency table already flags — against `ssd1309` at
0.4.0, ~13k, 2023-11 and `display-interface`-compatible. Small enough to own either
way, per `implementation.md`, but not nothing.

**So of the two I2C candidates it is the SSD1309** — the Gray4 argument and the crate
age hold without qualification, and the bus hitch is a third, softer reason. Note
which two carry it: the framebuffer depth and the maintenance state, neither of them
perceptual.

### But there is a decision *above* this one, and it is the ADC

Working out why the SSD1327 was expensive turned up the general form, which is worth
more than the verdict: **the constraint is I2C's bandwidth, not the panel.** 400 kHz
with 9 bits per byte is 44.4 kB/s, so a 30-50 ms redraw budget caps the framebuffer
at 1,333-2,222 bytes:

| | Bytes | Full frame | |
|---|---|---|---|
| 128x64 mono | 1,024 | 23.0 ms | comfortable |
| 128x128 mono | 2,048 | 46.1 ms | at the ceiling |
| 160x128 mono | 2,560 | 57.6 ms | over |
| anything 4 bpp | 8,192 | 184.3 ms | out |

So **on I2C the resolution is decided for you and the only free variable is physical
size.** 128x64 monochrome is the whole option space, and the 128x128 mono row is a
trap rather than an escape: the part exists — SH1107, I2C — but only at 1.12 inch,
which puts a 12x12 glyph at **1.89 mm**, smaller than the SSD1327 row this section
just rejected for being small. There is no third I2C panel to find.

On SPI the constraint simply disappears. The same 128x64 is **0.82 ms** at 10 MHz,
28x faster; SSD1322 256x64 is 6.55 ms at 10 MHz and 2.05 ms at 32; ILI9341 320x240
is 38.4 ms for a full frame at 32 MHz but **2.56 ms** for a 320x16 text band, and
partial updates are the normal case there.

**And "SPI collides with the v2 ADC" is true only for an SPI ADC.** `hardware.md`
names both candidates and identifies the choice as the lever for the button
ceiling — "MCP3008 is SPI and costs four pins, ADS1115 is I2C and costs none" — but
that analysis was never applied to the display axis, which it also decides:

| ADC | Display | Buttons |
|---|---|---|
| MCP3008 (SPI) | I2C, so 128x64 mono; pick 2.42" or 2.7" | 8 |
| **ADS1115 (I2C)** | **SPI is free — ILI9341 and SSD1322 both live** | **12** |

With the fader on an ADS1115, GPIO 8-11 free up and GPIO 7 stays with unity. A
write-only panel needs SCLK, MOSI and CS — 11, 10, 8 — plus DC and RESET. **So it
fits with no button given up**, with one thing to confirm rather than assume: DC
needs a real GPIO, and the candidates are 9 (MISO, unused by a write-only panel, but
claimed by the SPI pinmux unless the overlay releases it) and 4 (the spare). RESET
can often be tied high instead. Check the overlay parameters on the image, the same
caution `hardware.md` applies to `gpio-key`.

**The ADS1115 is also the better ADC on merit, which removes the tension.** It is
16-bit against the MCP3008's 10-bit, and its 860 SPS ceiling is ample for a fader
sampled at ~100 Hz. So the branch that frees SPI is the branch a pitch fader wants
anyway; the pin saving is not being bought with resolution.

### A second I2C controller exists, and it is worth having for the *other* panel

The BCM2837 has two. I2C1 is GPIO 2/3 and carries the WM8804 and, as designed here,
the display. **I2C0 is GPIO 0/1** — the HAT ID EEPROM pins, which the GPIO map
already counts as reserved and which this build already does not depend on, since
`config.txt` names the Digi2 Pro overlay explicitly rather than relying on HAT
autodetection.

A display on I2C0 would have its own controller and its own clock, leaving the codec
I2C1 to itself. **But be precise about what that dissolves.** It removes the
*contention*. It does not touch the 184 ms, which is bytes divided by bandwidth and
does not care whose bus it is on — a folder change still takes 184 ms. And it says
nothing at all about the Gray4 argument, which is the one that decides. So **it does
not reopen the SSD1327.**

Where it does pay is the panel most likely to actually be bought. An SSD1309 on I2C0
takes its 23 ms frames off the codec's bus entirely, which would make
`architecture.md`'s refresh discipline non-load-bearing **without going to SPI** — the
same second-order effect the SPI branch gets, for two pins that are already spent.

Three unknowns, all of the silent-when-wrong class, so none of this is adopted yet:

- whether `dtparam=i2c_vc=on` exposes GPIO 0/1 as `/dev/i2c-0` on a **3B+**
  specifically — i2c0 is also the VideoCore's bus and serves the camera and display
  connectors on this generation, so it is not simply a spare
- whether GPIO 0/1 carry the pull-ups a panel needs, which is the same question
  HiFiBerry raises about adding I2C slaves at all
- whether the firmware's boot-time EEPROM probe disturbs a panel sharing the lines.
  It should not — the probe addresses 0x50 and a panel answers at 0x3C/0x3D — but the
  HAT specification says those two lines are for the EEPROM and nothing else

One thing it would settle rather than risk: using I2C0 gives up HAT autodetection
**deliberately**, where the explicit overlay currently gives it up as a side effect.

### The Gray4 argument does not carry over to the ILI9341

Worth stating outright, because it looks as though it should. The SSD1327's four bits
per pixel are wasted because they would encode **intensity inside a glyph**, and a
1-bit BDF glyph has none. Colour is not the same thing: it encodes **which glyph is
which**, and rendering the same 1-bit glyph in a different colour is free. Unplayable
files marked red, folders distinct from files, the rate and depth in use in their own
colour — all of that is usable with exactly the font pipeline already planned. So
"more bits per pixel than a 1-bit font can fill" rules out Gray4 and says nothing
about the ILI9341.

Which reopens the row this table already rates highest — 20 chars, 13 rows, 2.9 mm,
cheapest, no burn-in — set aside only on a pin cost that turns out to be contingent
on a decision this project has not made. Two costs that stay real: the backlight is
always-on light in a dark room, and **dimming it wants a PWM pin or a fixed resistor**
where the OLED's blank command is free; and HiFiBerry's 3.3 V caution bites hardest
on a backlit panel, which `hardware.md` already answers by giving the display 5 V and
its own regulation.

**What is not settled is now one question, not three rows:** whether the v2 ADC is
I2C or SPI. Everything else follows. If it is unresolved when the panel is bought,
buying the I2C OLED forecloses the branch the file's own assessments point at on both
axes independently.

### What the rendering settled, and the one thing it cannot

`tools/panel-compare` draws the same folder listing at all six geometries. Four
things came out of the frames that no amount of arithmetic here had produced:

- **8x8 kanji are wrong, not small.** Visible, not inferred.
- **The 12x12 row counts were overstated by half** — two browsable rows on a
  128x64 panel, not four. This is the finding that hurts, because the I2C branch's
  case rested on those rows.
- **SH1107 at 1.89 mm is visibly out**, which closes the 128x128-mono route on
  legibility as well as on the geometry above.
- **Colour does the "say why, not just that" job for free.** In the ILI9341 frame
  the refused files — `圧縮済み.flac`, `32bit_float.wav` — are red and folders are
  blue, with the 1-bit font pipeline unchanged. On a mono panel the same information
  costs characters on a line where characters are the scarce thing. That is the
  concrete form of the colour argument above, and it is worth more than it looked.

**What it cannot settle is physical size**, because a frame on a desktop monitor is
at whatever scale the monitor makes it. The tool draws a 10 mm rule for exactly this
reason: **print it at 100% and measure the rule before judging any glyph size.** Until
that is done, treat the row counts, character counts and colour as settled and the
millimetres as arithmetic — the same split this file has been making all along, now
with the pixels drawn.

**One argument for the SSD1309 that does not hold.** It was put that 2.5 mm sits
below "the floor" this table asserts. It does not: the floor here is **12x12 pixels,
for glyph shape**, and no physical-size floor has been established anywhere. 2.5 mm
is merely the smallest row in the table — smaller than Japanese newspaper body text,
so the direction of the worry is right, but it is not a violated constraint.

**And the perceptual half needs no purchase.** `embedded-graphics-simulator` is
current, so the geometries render side by side on a desktop with real Japanese
filenames — same glyphs, same row counts, same truncation. That is the prototype step
suggested below, minus the panel, and it is free. With the ADC branch open it is
**three** geometries rather than two — 128x64, 256x64 and 320x240 — which is the
comparison that actually decides anything.

Fonts (all free, BDF): Misaki 8x8, Shinonome 12/16, k8x12 (8 px halfwidth /
12 px fullwidth, a good middle for mixed filenames). Rust reads BDF via the `bdf`
family of crates, or bake the glyphs to a bitmap atlas off the deck — which is
faster on an A53 and fits the project's own habit of moving work off the deadline.

**Confirmed, and it was a lead marked unverified here until the harness used it.**
`u8g2-fonts` is a maintained `embedded-graphics` text renderer built on U8g2, and it
bundles `u8g2_font_b12_t_japanese*` at a measured 12x12 and `b16_t_japanese*` at
16x16 — exactly the sizes this section calls the floor. That replaces the whole
BDF-or-atlas question with a dependency. **One catch:** the crate is MIT/Apache but
its README says outright that the fonts themselves are not — fine for a harness that
is never shipped, a real question for the deck. See
[#10](https://github.com/tamatebox/deck-pi/issues/10); the BDF fallbacks above stay
live because of it.

Suggested: prototype the UI on a cheap 0.96 in panel — unreadable, but it settles
what fits in how many pixels — then choose. That advice is now weakened: "what
fits in how many pixels" is arithmetic, and the thing a cheap panel cannot tell you
is legibility, which it misrepresents by being finer-pitched than the candidates.
Prototype against a **candidate**, not a stand-in. `embedded-graphics` keeps the choice
reversible: drivers exist for every controller listed above (ssd1306, ssd1309,
ssd1322 including a 256x64 variant, ssd1327, ili9341, st7789), and
`linux-embedded-hal` puts them on the Pi's `/dev/i2c` and `/dev/spidev`.

**2. libsoxr on A53** — unmeasured. Benchmark precision x output rate x pitch
range for realtime ratio *and* worst-case block time.

It no longer decides whether the board is viable. v1 has no resampler, so the 3B+
plays all six rates regardless; and in v2 a rate the resampler cannot sustain
falls back to **unity**, which is already a designed path and is bit-perfect.

**How much comfort that is depends on the material, and it is more comfort than one
draft of this row allowed.** That draft said losing pitch means losing beatmatching,
"the deck failing at its job for that track". **That was an inference, not a usage
fact** — see the premise row in Reversed. The deck is one deck, changing speed like a
record, with no second source to sync to. So losing pitch on a rate the resampler
cannot sustain means losing the **speed control** for that track: a real loss, more
than a nicety, and not a sync failure. The question keeps some teeth and not the ones
that draft gave it.

Still run it **before** the enclosure fixes the board, and run it **thermally
soaked**: the 3B+ throttles 1.4 -> 1.2 GHz at 60 C, so a cold run measures a clock
a set will not hold.

**The pitch-range axis is no longer an axis: it is ±10%.** Supplied by the user, so
it is a fact rather than a reading of the material. That replaces two wrong versions
of this paragraph — first "long-form work sits near `r = 1.0` and does not need the
club-width range", which assumed the material, and then "beatmatching means the full
range", which assumed the use. Neither was asked.

At a fixed ±10% the benchmark's dimensions reduce to **precision x source rate x
output rate**. And the range is unlikely to be the cost driver at all: the output
rate follows the source, so the number of output samples per period is *constant*
whatever `r` is — only input consumption varies — and what the range affects is the
anti-alias filter's cutoff, which barely moves across ±10%. `architecture.md` already
names the real driver, that a 192 kHz source costs about twice a 96 kHz one at the
same output rate. **The filter-length half of that is an estimate**; the constant
output-sample count is a consequence of the unity-rate invariant.

**It briefly had a second axis and no longer does.** A body of measurement said
`SOXR_VR` reallocates in the callback while the ratio moves; it was a misconfigured
probe and is retracted — see Reversed. What replaced it is a setup requirement rather
than a measurement: **declare the pitch range at `soxr_create`.** Nothing here needs
the Pi.

**3. ENTER as its own button?** Cheap encoder push switches bounce and wear, and
ENTER is the most-used control. Pin budget allows a dedicated button.

**4. One hardware fact still needing the boards.** Recorded in `docs/hardware.md`
under Sources - Unverified.

*The J12/J13 values are settled* — §F's table, read from a legible scan, agrees with
all six worked examples: master is J13 shorted 1-2 and 3-4, J12 open, both shunts
vertical. The manual was self-consistent all along; the apparent contradiction came
from reading the board silkscreen off an oblique photo.

*What the Digi2 Pro's "isolation ground jumper" does.* Undocumented, but the
datasheet's mention of an output isolation transformer narrows it to bonding or
floating the output ground — which decides whether the clean side is referenced
through the coax shield, and therefore feeds straight into question 6. This is the
one to chase first.

A third item, whether the Digi2 Pro has a pass-through GPIO header, is close to
answered: the datasheet enumerates its connectors and none is a pass-through. Treat
it as a terminating HAT, which is why the controls wait for the isolator's J4 in
bring-up phase C rather than needing a breakout earlier.

**5. Enclosure vs. the 60 C soft limit.** The official guidance is that a case
"should not be covered", and a sealed DJ enclosure covers it. A fan is an acoustic
noise source in a listening context and an electrical one next to the audio
boards; passive options are a vented enclosure, a heatsink, or accepting 1.2 GHz
as permanent (which the sizing already does). Depends on outcome 2.

**6. Power topology, beyond the clean-side budget.** Deliberately deferred, not
overlooked. `hardware.md` budgets the clean side and records J1's 3.3-5 V range,
the 4.8 V floor and the GPIO-header input, but three things are unworked: the Pi
side's own current budget, whether the clean supply's secondary must float (its
ground reference arrives through the S/PDIF shield from the DAC — which now depends
on the Digi2 Pro's `JP1`, see `hardware.md`), and power-on order (the isolator
driving SCK/LRCK into an unpowered Pi is the direction to worry about).

**Power-on order is now answered, and the answer is that it does not matter.** The
isolator IC reads as `CA-IS3760HW` on the board photo — a Chipanalog CA-IS376x, and
the `H` suffix means outputs default *high* when their input side is unpowered. The
manual's block diagram also shows the isolator's Pi-side rail coming from the Pi.
Together: with the Pi off, the Pi-side output stage is off too and can drive nothing
into the Pi, so the back-powering worry does not arise; with the clean side off,
SCK/LRCK simply sit high and there is no audio. Neither order damages anything.
Confirm the suffix on the actual chip, and note the block diagram implies more than
one isolator.

What is left is the grounding half plus the Pi-side current budget. The grounding
half was recorded as hinging on the Digi2 Pro's `JP1`; that now looks wrong. The
output transformer is a Pulse `T6074NL`, the **electrostatically shielded** variant,
and a shield does nothing unless grounded — so `JP1` most likely grounds that
internal shield, which is a noise-rejection choice and says nothing about where the
clean side takes its ground reference. Not certain, but a continuity check on the
bare board decides it rather than a vendor query. See `hardware.md`. Neither binds
until the isolator goes in, at bring-up phase C.

**7. How the deck starts at boot.** Nothing in any document covers it. The only
constraint on record is that the audio process needs `rtprio` and `memlock` and no
root, so it would be a non-root service — but whether it is a `systemd` unit, what
it does when no medium is present, and what happens if it dies mid-set are all
unaddressed. Small, but it is the difference between a program and an appliance.

## Phases

**v1** — browse and play. Unconditionally bit-perfect: with no pitch and no jog
there is no other mode, so no resampler and no unity button. Encoder, three
buttons, display.

**v2** — pitch fader (the Pi has no ADC at all; whether it is SPI or I2C is open
question 1's deciding axis), jog wheel, libsoxr, the unity button. The v1 read path,
control-thread shape and rate variable accept this without rework. **The window's
*filling policy* did not, and was the one thing that needed changing** — it appended
forward only, so a descending playhead was served 0 of 12 periods. It is now
direction-aware and serves 12 of 12. That is the whole of what "built to accept v2"
turned out to cost, and it was found by writing the test rather than by reading the
claim.

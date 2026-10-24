# This build

What is actually here, wired how. **Every other document is written to hold for any
build of this deck; this one is not.** It is the one place that says which parts
exist and what is connected, `CLAUDE.md` imports it so that every session starts
with it, and it changes in the same commit as anything that changes the bench.

Every fact is **supplied by the user** unless it says otherwise, and carries the date
it was true. A stale line here is a wrong fact, not an out-of-date opinion — date what
you change, and when a line's date is old and the question matters, ask.

## The boards

| | | |
|---|---|---|
| Pi | **`deck-pi`: `phono`'s board (Pi 4 Rev 1.5, 2 GB) booted from its own microSD** — Raspberry Pi OS Lite 2026-10-06, kernel 6.18.50, root read-only (*The bench*). Swap the card and the same board is `phono`, the user's vinyl-ripping Pi, again | 2026-10-24 |
| Digi2 Pro | Board release 2.2, mounted directly on the header | 2026-09-24 |
| IsolatorPi III | **Not fitted** | 2026-09-15 |
| Pico 2 H | Runs `firmware/`, reaches the Pi over USB | 2026-09-15 |

## The controls

| Control | Part | Pico | State |
|---|---|---|---|
| PLAY | CDJ-200 `KSWB`, CN602-6 | GP4, pin 6 | jumper wire; **reads** 2026-10-22 |
| CUE | `KSWB`, CN602-7 | GP5, pin 7 | jumper wire; **reads** 2026-10-22 |
| SEARCH, TRACK SEARCH, FOLDER SEARCH — two each | `KSWB` ladder, CN602-8 `KD2` | GP26 / ADC0, pin 31, the pad's pull-up and **no external resistor** | jumper wire; **all six read** 2026-10-22 |
| Their ground | `KSWB`, CN602-9 `GNDA` | GND, pin 3 | jumper wire |
| Browse encoder, push = ENTER | **not bought** | GP2, GP3, GP7 held for it | 2026-10-21 |
| BACK | **not bought** | GP6 held for it | 2026-10-21 |
| Pitch fader — v2 | CDJ-200 `SLDB` | GP27 / ADC1 held for it | **unconnected** 2026-10-22 — the Pico has one 3V3 pin and the panel has it |
| Jog — v2 | CDJ-200 `JOGB` | undecided | unconnected |

The connectors pin by pin, and every measurement taken through them, are in
`cdj-200.md`.

**`KSWB` takes four wires and no supply.** `CN602-3` (`V+5V`) and the LED pins 1, 2,
4 and 5 are left off: the buttons only ever pull to ground, and the pull-ups are the
Pico's (2026-10-22).

**Checked through `faderprobe`, from a Mac, 2026-10-22.** PLAY and CUE each read
three presses as three clean edges — after a first try in which PLAY read as CUE
and CUE as nothing, fixed by moving the jumpers. The ladder gave six distinct
plateaus, each inside its own bucket, and idle sat at 3952 as it did on
2026-09-18. **Two of the six levels moved by about 200 counts since then, and one
came within 35 counts of its boundary**, which moved to 2862 the same day —
`cdj-200.md` has the numbers. The jumpers are soldered at the `KSWB` end and
plugged onto the Pico's header.

**And through `phono`'s kernel the same day**: all eight buttons pressed as their
own keycodes, FOLDER's 403 and 402 included.

**GP9 and GP10 are read, and nothing is on them.** The firmware ORs a discrete SEARCH
pair on those pins with the ladder's; this build has none and is not getting one
(2026-10-21). Pulled up, they idle released.

**Nothing turns a detent yet**, because the encoder is not bought — so `REL_X` has
been declared to the kernel and never sent by a real knob.

**Until it is, FOLDER ►►| is ENTER** (2026-10-22, `src/input.rs`). With no encoder
the selection never leaves the top row, so on a flat folder it loads the first file,
TRACK SEARCH walks from there and PLAY plays. **Since 2026-10-23 the first file is
loaded when the medium arrives**, so on this build's flat folder PLAY works from boot
and FOLDER ►►| only reloads the highlighted row. And the selection follows TRACK
SEARCH, which began as a stand-in here and is now the deck's rule (`controls.md`).
The FOLDER mapping is the temporary half, and goes when the encoder's push is wired.

## The panel

An **SSD1322 3.12" 256x64 OLED** module, sold by WAVGAT, **blue** — its grey levels
are shades of blue. **Wired and showing the `paneltest` card correctly** on
2026-10-22: border on all four edges, the corner marker where it was drawn, and
the ramp in steps, at 4 MHz over jumper wires from the Pico's 3V3. That settles the
two things the controller's datasheet cannot: the glass starts at **RAM column 28**
and wants **`0xA0` = `0x06, 0x11`** — u8g2's Newhaven values, not its ZJY ones.
That answers [#2](https://github.com/tamatebox/deck-pi/issues/2) by purchase rather
than by its comparison.

**The deck draws on it** (2026-10-22): the shipped firmware serves it over USB, and
the deck on `phono` found the link, was declared 256x64, and put `No USB` on the
glass. **SPI runs at 9.375 MHz since 2026-10-23** — the SSD1322's 10 MHz, as near as
the divider gets — and the screen came up clean over the same jumpers, with a
frame's round trip down from 30.3 ms to 20.2 ms.

**One boot came up faint and squashed** (2026-10-24, the user's eyes): frames were
being sent and acked, so the deck and the link were fine, and the next reboot was
clean. That is the look of a lost `0xCA` — its reset value is 128 MUX on 64-row
glass (Table 9-1, read) — whether the init half-landed or the controller was reset
afterwards with the Pico still running; nobody saw the panel at power-on, so which is
not known. **The firmware now mends either**: it runs the whole init again when a
deck says hello, seconds after power-on, and writes every setting, the contrast and
display on or off again before every frame. The user saw no flicker from it, in
playback's once-a-second redraw or in a sliding name (2026-10-24). The ELF it
replaced is `~/deck-bin/deck-pico.prev`.

- **The header is seven pins, SPI only:** `GND VCC SCK SDA RES DC CS` (2026-10-22).
  The SSD1322 chooses parallel or 4-wire SPI on `BS0` and `BS1` — both low for
  4-wire SPI, per Orient Display's datasheet for its own SSD1322 module — and this
  board brings out no parallel data lines at all, so it is set for 4-wire SPI.
  **Inferred from the header**, not read off the select pins. `SDA` is SPI data in,
  not I2C.
- **It runs from the Pico's 3V3**, so 3.3 V is enough for it. The Pico 2 datasheet says the pin "can be
  used to power external circuitry" and recommends under **300 mA** (§3.1). Two things
  are owed before it is relied on: the panel's own draw at full white, **unmeasured**,
  and its effect on the ADC, which reads against that same rail filtered through
  201 Ω into 2.2 µF (§5.3). `faderprobe` with the panel blank and then full white,
  against the 2026-09-18 figures in `cdj-200.md`, is that measurement. GPIO23 high
  forces the regulator into PWM and cuts its ripple, if it comes to that.
- **Pins, wired 2026-10-22:** SPI0 — `SCK` GP18 (pin 24), `SDA` GP19 (25),
  `CS` GP17 (22), `DC` GP16 (21), `RES` GP20 (26), `GND` pin 23, `VCC` 3V3 (36).
  The SPI functions were read against `embassy-rp` 0.10's pin table, not recalled.

## Pins on the Pico

| | |
|---|---|
| In use | GP4, GP5, GP26 (`KSWB`) |
| Held for parts not bought | GP2, GP3, GP6, GP7 |
| Held for a part not connected | GP27 (fader) |
| Read, nothing attached | GP9, GP10 |
| The panel | GP16, GP17, GP18, GP19, GP20 |
| Free | GP0, GP1, GP8, GP11-GP15, GP21, GP22, GP28 |

## The medium

One real stick, mounted read-only at `/media/stick`: **2192 names** (2026-09-16), and
**no 192 kHz material**, which is why `tests/deck_soak.rs` has only run at 44.1 kHz.

## The bench

- **Builds happen on a Mac.** Linux binaries are built there in Docker
  (`rust:slim-trixie` with `pkg-config libsndfile1-dev libasound2-dev`) and copied to
  the Pi, which also makes the container the Linux gate when no Pi is reachable.
- **`deck-pi` is the deck's own card**, reflashed 2026-10-24: Lite plus `libsndfile1`,
  `picotool` and `overlayroot`, the realtime limits in
  `/etc/security/limits.d/99-deck-pi.conf`, journald volatile
  (`/etc/systemd/journald.conf.d/volatile.conf`), and `~/deck-bin` holding the deck,
  the bring-up CLI, the Pico's ELF and the scripts below. `sudo` asks for a password
  and then holds for a while across sessions (`timestamp_type=global`), so a root
  step is the user's or follows their `sudo -v`. On Wi-Fi; the cable is for when
  that fails.
- **The card is three partitions, and the root is read-only** (2026-10-24, because
  the deck is switched off by pulling the power — `decisions.md`): boot 512 MiB,
  root 12 GiB, and `/data` (`LABEL=deckdata`, ext4, 45.7 GiB, `nofail` so a
  damaged one leaves a booting Pi to ssh into — systemd has it wanted, not required,
  by `local-fs.target`; untried against a real failure) for everything
  written. `~/music`, `~/deck-bin` and `~/.local/state/deck-pi` are links into
  `/data`, so music syncs, deck updates and cues need no reboot. The root is
  `overlayroot=tmpfs:recurse=0` at the head of `cmdline.txt` — `recurse=0`, or
  `/data` would be read-only too — and `/boot/firmware` is `ro` in `fstab`.
  **Anything written outside `/data` is in RAM and gone at power-off**, silently:
  measured the same day, a file in `~` vanished across a reboot and one in `/data`
  stayed. The clock's saved time goes the same way, so each boot starts where the
  last one started until NTP corrects it; the deck uses no wall time. **Pulled twice
  mid-playback** the same day: each time the deck came back with its first track
  loaded and `SCHED_FIFO` 75, the Pico enumerated once, and `/data` passed its boot
  check — the second time after replaying its journal, which is what ext4 does for
  a volume the power left mounted. **To change the OS**: `sudo mount -o
  remount,rw /boot/firmware`, take `overlayroot=tmpfs:recurse=0 ` out of
  `cmdline.txt`, reboot, change it, put it back, reboot. raspi-config's own
  overlay switch writes and removes only `overlayroot=tmpfs`, so it cannot undo
  this one. **Or, for a small edit, `sudo overlayroot-chroot <command>`**, which runs
  it in the lower root remounted writable — how `nofail` went in, 2026-10-24.
  **A reboot always follows it.** The kernel's overlayfs document says changes to
  an underlying filesystem while the overlay is mounted "are not allowed" and leave
  the overlay's behaviour undefined, so what the running system sees of the edit is
  not to be trusted until the next boot. It also could not remount the lower
  read-only afterwards ("mount point is busy") — likely `sed -i` having replaced a
  file the overlay still held, untested — and the reboot ends that too. Anything
  larger than an edit, a package for one, takes the two-reboot way above.
  `/etc/fstab` as seen is overlayroot's copy, rebuilt from the lower one at boot.
- **The Pico is flashed from the Pi, over ssh, with no button and no `sudo`**:
  `picotool reboot -f -u`, then `picotool load -x -t elf ~/deck-bin/deck-pico`.
  The Debian package's udev rule is what lets `plugdev` at it. The ELF is built on
  the Mac and copied over; `picotool` wants `-t elf`, the ELF having no extension.
  A reflash breaks the deck's panel link, so the deck is restarted after one.
- **Music is prepared on the Mac in `~/Music/DECK`**, outside this repo like the
  stick-preparation it stands in for: `tracklist.txt` and `extra/` in, `./deck.sh`
  converts what the list names (rate and depth kept, PCM checked against the
  source) and numbers it in list order, takes `extra/` exactly as it is — names,
  contents, nothing checked — mirrors the lot to `deck-pi:~/music/DECK` and
  restarts the deck.
  It is also where `run-sd.sh`, `boot.sh` and `deck-pi.service` live, installed by
  `./deck.sh install`.
- **26 files on it** (2026-10-24): 21 from `tracklist.txt`, ALAC converted to AIFF,
  and the user's 5 from `extra/` — 44.1 to 96 kHz, 16 and 24 bit, 4.1 GB. The folder on this
  card is run as a medium by **`~/deck-bin/run-sd.sh`**: the deck
  takes only a mount point (`src/media.rs`), so inside `unshare --user
  --map-root-user --mount` it mounts an empty tmpfs and bind-mounts the folder
  read-only beneath it, and the mount point's device differs from its parent's. No
  `sudo`, and nothing shows outside the namespace. **Realtime survives it**: with a
  track loaded, the audio thread was `SCHED_FIFO` 75 and the other two were not
  (`ps -L`, measured 2026-10-23). Temporary, like the FOLDER
  mapping above; a stick and the udev rule are the deck's own way.
  **`~/deck-bin/boot.sh`** is the boot wrapper, waiting up to a minute for the
  Pico's link and the Digi2 Pro, because a deck that starts without its panel stays
  on the console — one that loses it later finds it again (`README.md`).
- **The deck is `deck-pi.service`** (2026-10-23, the crontab entry removed): systemd
  runs `boot.sh` as the user, with `Restart=always`, and the realtime limits in the
  unit. Its log is `journalctl -u deck-pi`. **To restart it, signal the deck** —
  `./deck.sh restart` sends `SIGINT` and systemd starts it again — and never start
  `boot.sh` by hand, which makes a second deck wanting the same Digi2 Pro. Measured
  the same day: back in about 2 s after `./deck.sh restart` and after a `SIGKILL`,
  each time with the audio thread at `SCHED_FIFO` 75.
- **`phono`'s own card is unreliable.** On 2026-10-22 it dropped off the bus
  mid-write (`mmc0: card removed`, then `error -84`), and its two netplan files were
  found empty the next day, which is what took its Wi-Fi. Very likely the same
  dropout is what reset every new ssh connection earlier that day. Its journal does
  not survive a reboot, so none of it is certain. It is the vinyl machine again,
  with the deck's boot entry removed.
- **Hold one ssh connection open** (`ssh -M`) when the card under the Pi is in doubt:
  a held connection keeps working when new ones cannot start.

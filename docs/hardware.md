# Hardware

## Board stack

```
Raspberry Pi 3B+
   |  40-pin GPIO
IsolatorPi III        <- galvanic isolation (5 kV), J1 clean-side power in
   |  isolated I2S + I2C
HiFiBerry Digi2 Pro   <- WM8804, dual-domain clock, master mode
   |  S/PDIF (RCA coax, 75 ohm)
external DAC
```

The Digi2 Pro's bundled M2.5x12 mm spacers assume it mounts straight onto a Pi.
The IsolatorPi III sits between them, so longer standoffs are needed.

## The Pi

Raspberry Pi 3 Model B+. Same 40-pin header and the same mechanical footprint as
the 3B, so the GPIO map, the HAT stack and the standoff plan do not depend on
which of the two it is.

| | |
|---|---|
| SoC | Broadcom BCM2837B0, quad Cortex-A53, **1.4 GHz** |
| RAM | 1 GB |
| Ethernet | Gigabit, **over USB 2.0** — 300 Mbps maximum |
| Wireless | 802.11b/g/n/ac dual-band 2.4/5 GHz, BT 4.2/BLE — both disabled here |
| USB | 4 x USB 2.0 on one shared bus |
| Power in | 5 V / 2.5 A micro-USB, **or 5 V via the GPIO header**; PoE needs a separate HAT |
| Operating temp | 0-50 C |

Supply voltage must stay **above 4.8 V** for reliable operation, and the official
docs warn that some USB supplies sag to 4.2 V because they are designed to charge
a 3.7 V LiPo rather than run a computer. `vcgencmd get_throttled` reports
undervoltage, but only after it has happened.

### 1.4 GHz is a sprint clock, not the planning number

The 3B+ (with the 3A+) is the only model with a **soft** temperature limit, and it
exists specifically to bound how long the board holds its headline clock:

> When the soft limit is reached, the clock speed is reduced from 1.4 GHz to
> 1.2 GHz, and the operating voltage is reduced slightly. ... we trade a short
> period at 1.4 GHz for a longer period at 1.2 GHz. By default, the soft limit
> is **60 C**.

`temp_soft_limit` raises it, but to 70 at most, and the documentation says that
"might cause instability". The hard limit is unchanged — progressive throttling
from 80 C, Arm and GPU throttled at 85 C.

A deck runs a continuous realtime load, inside a box, for the length of a set.
The steady state is therefore **1.2 GHz**, and that is the figure
`architecture.md` sizes against. Not because the board is a 3B — because 1.4 GHz
is not sustained. Going 3B to 3B+ buys thermal mass and Gigabit Ethernet; it does
**not** buy 17% of resampler headroom.

It also settles how the v2 libsoxr benchmark must be run: **thermally soaked, at
the throttled clock.** Started from cold it reports the sprint figure, and would
pass a board that fails twenty minutes into a set — this project's usual failure
shape, silent and late.

Set against that, the official case guidance: "if used inside a case, the case
should not be covered." A sealed DJ enclosure is in tension with it, and a fan is
both an acoustic and an electrical noise source. Open — see `decisions.md`.

## Assembly checklist

Three things are easy to get wrong and produce no error when wrong.

1. **Set J12 / J13 on the IsolatorPi III to master mode.** The default is *slave*,
   in which the Pi generates the I2S clock from its own PLL — the high-jitter path
   this whole build exists to avoid. In master mode the Digi2 Pro's two crystal
   oscillators generate SCK/LRCK and feed them back to the Pi, which then only
   generates DATA. **Audio plays either way**, so nothing surfaces the mistake.

   **Master mode is J13 shorted, J12 open.**

   | | J13 | J12 |
   |---|---|---|
   | Slave (default) | open | 1-2 and 3-4 shorted |
   | **Master — use this** | **1-2 and 3-4 shorted** | **open** |

   §F's table and all six of §I's worked examples agree on this, so the manual is
   self-consistent. Application example 1 is the directly applicable one, being for
   "any WM8804/5 based Transport/DAC" — which the Digi2 Pro is.

   **Both jumpers go on vertically.** The pins are 3 and 1 across the top, 4 and 2
   across the bottom, so `3-4` bridges the left column top-to-bottom and `1-2` the
   right column. Two vertical shunts side by side. Placing them horizontally
   (1-3, 2-4) is the wrong **orientation** that §J-4 warns can *damage* the board —
   a harder failure than the silent one above, so check before applying power.

   An earlier version of this file refused to record the values, because the board
   silkscreen looked like it said `SLAVE` beside J13 and `MASTER` beside J12. That
   reading came from an oblique photo and was wrong. Still worth a glance at the
   flat board, but two independent sections of the manual agreeing outweigh it.

2. **Feed J1 with clean 5 V, and never power the isolated side from the Pi.**
   J1 is the clean-side input; it regulates the isolator and passes the supply
   through to the audio board on J6 pins 2 and 4. Powering from both sides bridges
   the isolation and defeats the build. The Digi2 Pro's own 5 V connector is then
   unused.

   J1's stated range is **3.3-5 V**, and Ian's own application examples feed it
   3.3 V — but those drive DACs that run on 3.3 V. Here it must be 5 V, because
   the same rail passes straight through J6 pins 2/4 to a Pi HAT that expects
   5 V.

   J1 is a **green 2-pin screw terminal**, silkscreened `CLEAN POWER` with a `⊕`
   marking the positive and `3.3/5V` beside it, and the kit ships red and black
   pigtail leads for it. Read off the board photo in the Sources list, so no
   guessing about polarity is needed.

3. **Leave GPIO 5 and 6 free.** J6 pins 29/31 carry them through as the
   oscillator-select lines for master mode. The Digi2 Pro overlay names them
   outright — `clock44-gpio = <&gpio 5 0>` and `clock48-gpio = <&gpio 6 0>` — so
   **GPIO 5 enables the 44.1 kHz crystal and GPIO 6 the 48 kHz one**, and the
   machine driver switches them from `hw_params` on every rate change.

   The failure mode is worse than "oscillator select stops working". If the driver
   cannot get those GPIOs it falls back to `sysclk_freq = 27000000`, and 27 MHz is
   the WM8804's **PLL** reference. So audio still plays — through the PLL, which is
   the high-jitter path this whole build exists to avoid. Silent, again.

## Bring-up order

Fit the isolator **last**. This is not a preference — the IsolatorPi III manual
(§J-1) says to validate that the hardware and software work and produce audio
*before* installing the isolator between the Pi and the audio card, because
debugging is much harder once it is in.

The audio half and the control half are independent, and **neither waits for the
other or for the isolator.** The only constraint is physical: the Digi2 Pro is a
terminating HAT, so it fills the 40-pin header and the two halves cannot be on the
Pi at the same time until J4 exists. So swap the HAT on and off and do them in
whichever order the parts arrive in.

**A — bare Pi, no audio hardware.** Buttons and the encoder wired straight to the
40-pin header, a cheap panel on I2C, a stick in a USB port. This is the input path,
the browser, the display, media watch, the cue store and the file layer — every
module except the audio engine. It also settles open questions 1 (what fits in how
many pixels) and 3 (whether ENTER wants its own button). Note that the null test
needs no audio hardware either: it compares buffers against the source, so it runs
here, or on any machine.

**B — Pi plus Digi2 Pro, audio only.** The bundled M2.5x12 mm spacers are the right
length, so nothing extra is needed, and `dtoverlay=hifiberry-digi-pro` is already
explicit so `config.txt` does not change later. Confirm S/PDIF out at every rate
and the `hw:` device. No controls here — nowhere to put them.

**C — Pi, isolator, Digi2 Pro.** Integration. Longer standoffs, J12/J13, clean 5 V
on J1, the grounding question, and the controls moving to J4, wired once into their
final home.

Two notes on the ordering. Controls belong in C rather than before it: reaching the
header past the HAT would need a GPIO splitter and then rewiring onto J4 afterwards,
and the manual's insistence on validating first (§J-1) is about **audio**, not
controls. And one integration risk appears only in C — the display and the WM8804
share the I2C bus. In A the display has it to itself, so redraw timing that felt
fine there can stumble once the codec is competing for the same bus.

Open question 2, the libsoxr benchmark, needs none of this. Bare Pi, no HAT, no
stick, no panel — so it can run before any of the three.

Two things do *not* change between B and C, and are easy to get wrong
by assuming they do:

- **GPIO 5 and 6 stay reserved.** They are Pi GPIOs routed *through* the isolator
  to the audio card — J6 pins 29/31 are documented as "XO selection — isolated
  GPIO5 and GPIO6". Removing the isolator does not free them. `decisions.md`
  records this being got wrong once already.
- **Clock ownership.** The Digi2 Pro's crystals are the clock master either way.
  J12/J13 exist because the isolator's channels are one-directional and have to
  be told which way the clocks flow; with the board direct-mounted there is
  nothing to select. What the isolator adds is ground and power separation, not
  clock purity — the manual is blunt about this in §J-2: the isolator "MAY NOT
  improve sound quality" by itself; it makes a good clean supply and good clocks
  count for more.

So A and B together are the whole v1 software stack, but B is **not** an
audio-quality baseline. Anything measured or listened to there does not carry to C.

## Power budget (clean side)

| | Draw | Provenance |
|---|---|---|
| IsolatorPi III | ~100 mA | manual §E — but see below |
| Digi2 Pro | <0.3 W (~60 mA) | datasheet |
| **Total** | **under 200 mA at 5 V** | neither figure is measured here |

That 100 mA is the only current number the IsolatorPi III manual gives, and it is
quoted for the board **with the DoP decoder daughter board fitted** — which this
build deliberately does not fit. The bare board draws less, and the manual does
not say how much less. So the budget errs in the safe direction, but it is not a
measurement of this configuration.

J6 also offers an **isolated, regulated 3.3 V / 200 mA output** on pins 1 and 17,
and 4.7 k pull-ups to 3.3 V on pins 15 and 22 for audio cards that need them.
Neither is used here; both are worth knowing before adding anything clean-side.

The isolator carries two indicator LEDs of its own, which the "no LEDs" decision
does not remove: **D1** lights when the Pi side has power, **D3** when clean
power is present at J1 or J6. D3 is a free answer to "is the clean side actually
up?", which is otherwise invisible.

0.5 A is ample. Both boards regulate their own input (the Digi2 Pro has an onboard
low-noise linear regulator), so a plain good-quality linear 5 V supply is enough —
the low current makes a quiet supply easy rather than expensive.

## GPIO map

**Reserved — 12 pins**

| GPIO | Use |
|---|---|
| 0, 1 | HAT ID EEPROM (physical pins 27/28 — *pins*, not GPIOs, a documented trap) |
| 2, 3 | I2C — WM8804 control, and the display |
| 5 | 44.1 kHz crystal enable (`clock44-gpio`) |
| 6 | 48 kHz crystal enable (`clock48-gpio`) |
| 14, 15 | Serial console (PL011, freed by `disable-bt`) |
| **18, 19, 20, 21** | I2S — **four** pins |

**GPIO 20 is I2S, not spare.** HiFiBerry's own GPIO-usage page reserves 18-21
(physical 12, 35, 38, 40) for the sound interface on the Digi2 Pro and says they
cannot be used for anything else. An earlier version of this table listed only
18/19/21 and left 20 free, and a button was assigned to it — the same mistake the
GPIO 5/6 invariant exists to prevent, on a different pin. GPIO 20 is PCM_DIN;
unused for playback-only, but claimed by the interface regardless.

GPIO 16 *is* free here. HiFiBerry reserves it on the plain Digi+, but the Digi+
Pro / Digi2 Pro entry replaces that with GPIO 5 and 6.

**SPI0 (7, 8, 9, 10, 11) is held for a future ADC.** The v2 pitch fader is analog
and the Pi has no ADC, so an MCP3008 or ADS1115 will be needed. Putting the
display on I2C instead of SPI is what keeps this option open.

**Assignment**

| GPIO | Use | Phase |
|---|---|---|
| 17, 27 | Browse encoder A / B | v1 |
| 22 | Encoder push — ENTER | v1 |
| 23 | BACK | v1 |
| 24 | PLAY / PAUSE | v1 |
| 25 | CUE — tap to set or jump, hold to preview | v1 |
| 16 | REW — hold to seek back, tap for previous | v1 |
| 26 | FF — hold to seek forward, tap for next | v1 |
| 12, 13 | Jog encoder A / B | v2 |
| 4 | spare | |

**One spare, not two.** Losing GPIO 20 to I2S costs a pin, so open question 3 (a
dedicated ENTER) would take the last one. If more are needed, the reserve is
**GPIO 7** — it is SPI0's second chip select, and a single ADC needs only one, so
7 can come out of the SPI0 block without giving up the v2 pitch fader.

Three cautions from HiFiBerry's GPIO-usage page, all of which this build touches:

- **"Do not use more than a few mA from the 3.3V line."** They ask for 5 V plus a
  regulator instead. A small OLED at 10-25 mA is already past "a few"; the ILI9341
  TFT option in open question 1, with a backlight, is far past it. So the display
  gets 5 V and its own regulation, not the 3.3 V pin — and that is a constraint on
  the panel choice, not an afterthought.
- **The I2C bus is shared with the WM8804, and HiFiBerry does not recommend adding
  slaves to it.** This design does exactly that. Their stated reason is pull-ups:
  "there might or might not be the right pull-up resistors on every I2C slave".

  The isolator largely answers that one, though. Its block diagram puts a Control
  I2C Isolator between the two sides, and J6 carries dedicated pull-up pins (15/22,
  4.7k to 3.3Vcc), so I2C is **two electrically separate segments** — the display on
  J4 sits on the Pi's segment, the WM8804 on the isolated one, and neither loads the
  other. Read from the block diagram and those pins rather than stated outright, so
  still worth confirming with a scope on the real stack.

  What does *not* go away is **bus time**: one logical bus from the Pi's controller,
  so a 26 ms full frame still shares it with WM8804 commands. That is the reason for
  the refresh discipline, not noise.
- **The whole stack is outside HiFiBerry's supported configuration.** They do not
  guarantee interoperability with other add-on cards, and the IsolatorPi III is an
  interposer rather than a direct plug. Ian Canada's manual supports the Digi Pro
  in master mode explicitly, so the combination is sound — but there is no vendor
  support for it from either side of the sandwich.

**Where J4 actually is.** `J4` is a reference designator silkscreened on the
IsolatorPi III — `J` for connector, the numbers not sequential by position. The
board carries three 40-pin connectors:

```
   ┌────────────────────────────────┐
   │ J13   [U1 isolator]   J12      │
   │         SLAVE      MASTER      │
   │  ┌──────────────────────────┐  │
   │  │ J6  ISOLATED GPIO        │  │  <- the Digi2 Pro plugs here
   │  ├──────────────────────────┤  │
   │  │ J4  NON-ISOLATED         │  │  <- controls and display here
   │  └──────────────────────────┘  │
   └────────────────────────────────┘
        (J3, the socket onto the Pi, is on the underside)
```

J4 and J6 are two upward-facing male pin headers **side by side** in the lower half
of the board, not stacked — the photo shows `J4 NON-ISOLATED` printed beside it with
pin 39/40 at one end and 2 at the other. Being male pins facing up, whatever
connects to J4 needs a female socket, which is what the IDC ribbon above provides.

The isolator is **65.5 mm** deep against a standard HAT's 56 mm, and J4 sits at the
outer edge, so J4 should fall outside the Digi2 Pro's footprint and stay reachable
with the stack assembled. Deduced from the dimensions and the photo — confirm on the
boards.

Control peripherals connect to the IsolatorPi III's **J4**, the non-isolated 40-pin
passthrough. The manual names rotary encoders as an intended use. Anything hung
there is on the near side of the isolation gap, so its noise never reaches the
audio boards — which is why the display needs no noise mitigation of its own.

## Controls

**v1** — one detented rotary encoder (EC11 class; the clicks are an asset for
menu stepping) plus five buttons: BACK, PLAY/PAUSE, CUE, FF, REW. ENTER is
the encoder's own push switch.

Cheap encoder push switches bounce badly and wear out, and ENTER is the most-used
control. Either raise the debounce interval to 30-50 ms or give ENTER its own
button — the pin budget allows it. PLAY takes the most abuse and deserves a
switch with real travel; BACK can be a small tactile. FF and REW get held down for
seconds at a time, so pick switches that are comfortable to hold rather than
crisp.

Buttons pull to ground and use the internal pull-ups. No external resistors.

### Wiring them

Two wires per button: one terminal to its GPIO, the other to any ground pin — the
header has eight. Grounds can be shared, so v1 is about **nine signal lines plus a
ground**: encoder A/B, its push, and five buttons.

**The numbering is the trap.** GPIO number is not physical pin number, as
HiFiBerry's own page warns, and the physical pins alternate odd and even across the
two rows:

| GPIO | Pin | | GPIO | Pin |
|---|---|---|---|---|
| 17 encoder A | 11 | | 24 PLAY | 18 |
| 27 encoder B | 13 | | 25 CUE | 22 |
| 22 ENTER | 15 | | 16 REW | 36 |
| 23 BACK | 16 | | 26 FF | 37 |

Verify with **`evtest`** rather than assuming: it prints `/dev/input` events, so a
press either produces the expected keycode or it does not, and the fault is either
the wiring or the overlay.

**Phase A** wants a labelled **GPIO breakout** to a breadboard — the printed pin
names are what stop the miscount, and tactile switches sit in a breadboard properly
where a jumper socket on a 2.54 mm leg does not.

**Phase C** wants something that cannot shake loose, because the same lack of a
latch that makes a knocked USB connector plausible applies to internal wiring. A
**screw-terminal breakout** is the answer: panel wires screw in, no crimping, and
it stays put. Take J4 out to it on a **40-pin IDC ribbon** and mount it elsewhere in
the enclosure rather than stacking anything on J4 itself — see the note on J4's
position below. Only about ten of the forty pins are used, so a full breakout is
overkill but cheap and harmless.

Use **stranded** wire anywhere it flexes between panel and board; solid wire
work-hardens and breaks. 26-28 AWG is ample for a switch carrying microamps.

### CUE

Three behaviours on one button, taken from the **CDJ-350** operating instructions
(Pioneer 389414-01U, p.18) rather than from memory. The 350 is the right reference:
an entry-level single player, closer in scope to five buttons than a CDJ-3000X with
its hot cues and touchscreen.

| State | Tap CUE | The manual's name |
|---|---|---|
| Paused | **sets** the cue point at the paused position | Setting Cue |
| Playing | **returns** to the cue point and pauses there | Back Cue |
| Held at the cue point | **plays while held** | Cue Point Sampler |

Four details worth having exactly, all quoted or paraphrased from that page:

- **One cue point per track.** "When a new cue point is set, the previously set cue
  point is canceled." So this is a single point, not a set of hot cues.
- **Setting it makes no sound.** "No sound is output at this time." Which agrees
  with FF/REW being a silent seek — nothing in v1 produces audio at a rate other
  than unity.
- **Back Cue pauses; it does not resume.** "The set immediately returns to the
  currently set cue point and pauses." Playback restarts only when PLAY is pressed,
  and it starts from the cue point.
- **The preview really is momentary.** "Playback continues while the button is held
  in" — so release means stop and return, and there is no latching.

**There is no separate STOP, because a CDJ has none.** Returning to the cue point
and standing by *is* stopping, which is why this button was labelled "CUE / STOP"
and is really one function. It also means the hold gesture is free for preview
instead of being spent on a stop the transport already has.

No new mechanism is needed: hold is `r = 1.0`, release is `r = 0` with the position
set back to the cue point. Both already exist.

For long-form material this is the main way to navigate *inside* a track, not a
mixing tool — which is why cue regions are pre-locked (see `architecture.md`).

**Auto cue is deliberately not adopted.** The CDJ-350 has it: on load it skips the
silent lead-in and places the cue point just before the sound starts, with eight
selectable thresholds from -36 to -78 dB. For club material that is a convenience.
For long-form ambient it is a hazard — a piece may open below -78 dB on purpose, and
having the deck decide where the music "really" begins is exactly the kind of
silent, well-meant alteration this project avoids. The cue point starts at frame
zero unless set.

Fine-adjusting the cue in single frames, which the 350 does with its SEARCH buttons
while paused at the cue, would fall naturally to FF/REW in the same state. Not
needed for v1, but the gesture is free if it is ever wanted.

### FF and REW

Hold to seek, tap to change track. This is what makes long tracks usable: without
it the only entry point into an 80-minute piece is the beginning, since the jog is
v2.

**This compresses two of the CDJ-350's controls into one pair, deliberately.** That
player separates them: SEARCH (`◄◄ ►►`) scans within a track, TRACK SEARCH
(`|◄◄ ►►|`) skips between tracks — four buttons where this deck has two. Tap versus
hold is the compression the pin budget asks for, and it is worth knowing it is a
compression rather than the idiom.

It also removes a worse idea. The alternative was to overload the browse encoder —
browsing in the list, seeking during playback — which puts a hidden mode on the
most-used control. Two dedicated buttons cost two spare pins and no mode.

**Seeking is silent in v1.** Position advances while held and the display follows,
but no audio is produced. This is not a UX preference: an audible scan needs the
resampler, which would give v1 a second mode and break the unconditional
bit-perfection `architecture.md` claims for it. In v2 it becomes `r = 4` on the
existing rate variable, and the unity button already owns the mode question.

Tap-versus-hold is discriminated in userspace, which does **not** contradict the
kernel-decoding rule below: that rule is about a poll loop quantising jog velocity
and dropping steps, whereas this is a one-shot timer per keypress. Two
consequences worth knowing — the tap action fires on *release*, which is
imperceptible for a track change; and the hold threshold (~300-500 ms) must sit
well clear of the 30-50 ms debounce interval.

Open: what a tap does at a folder boundary — stopping is the simple answer. A
track reaching its end is settled: it stops, nothing advances on its own.

**v2** — a non-detented *optical* encoder for the jog. Detents are disqualifying
here: the notches are felt through the platter while scrubbing. 100-200 PPR
(400-800 counts/rev after x4 decoding) is enough since there is no scratching.
Resolution sets the feel, not the audio quality — the rate slew in the resampler
absorbs coarse input. Below ~400 counts/rev, low-speed velocity estimation breaks
down: events arrive too far apart to tell how fast the platter is moving.

Plus a pitch fader, which needs the SPI ADC noted above.

## Wireless

Off. Ethernet is the only network path, so **confirm wired connectivity before
disabling anything** — the web-free UI is local, but SSH is the only way in.

```ini
dtoverlay=disable-wifi
dtoverlay=disable-bt
```

```sh
sudo systemctl disable --now hciuart bluetooth
```

On a Pi 3B+, Bluetooth occupies PL011, the good UART, leaving the serial console
on the mini-UART — whose baud rate tracks the core clock. `disable-bt` moves
PL011 to GPIO 14/15, so turning Bluetooth off and getting a solid serial console
are the same action. The 3B+ radio is dual-band, so this drops a 5 GHz
transmitter as well as the 2.4 GHz one.

The older note here — that `enable_uart=1` pins `core_freq` to 250 MHz — **is not
confirmed for the 3B+**; the current `enable_uart` documentation does not mention
`core_freq` at all. It also matters less once PL011 is in use, since PL011 does
not take its baud rate from the core clock. What *does* still ride the core clock
is **I2C**, and that bus carries both the WM8804 and the display.
`core_freq_fixed=1` is the documented lever — it "ensures that any peripherals
that use the core clock will maintain a consistent speed". A candidate, not a
decision, until measured.

During development, toggle with `rfkill` (state persists across reboots via
systemd-rfkill) and only commit to the overlays once measured. `config.txt` lives
on the FAT partition, so a mistake there is recoverable by reading the card on a
Mac; a mistake in the ext4 rootfs is not.

## config.txt

```ini
dtoverlay=hifiberry-digi-pro
dtoverlay=disable-wifi
dtoverlay=disable-bt
enable_uart=1
gpu_mem=16
```

Set the Digi2 Pro overlay explicitly rather than relying on HAT auto-detection —
the ID EEPROM lines may not survive the isolator.

`gpu_mem=16` reclaims ~50 MB on a headless box.

Button and encoder overlays are one instance per device; check parameter names
with `dtoverlay -h gpio-key` and `dtoverlay -h rotary-encoder` on the actual
image before trusting the spelling.

```ini
dtoverlay=gpio-key,gpio=23,keycode=158,label=BACK
dtoverlay=gpio-key,gpio=24,keycode=164,label=PLAYPAUSE
dtoverlay=gpio-key,gpio=25,keycode=128,label=CUE
dtoverlay=gpio-key,gpio=22,keycode=28,label=ENTER
dtoverlay=gpio-key,gpio=16,keycode=168,label=REW
dtoverlay=gpio-key,gpio=26,keycode=208,label=FF
```

168 and 208 are `KEY_REWIND` and `KEY_FASTFORWARD` — the *held* meaning, since one
pin carries one keycode and the tap meaning is a userspace interpretation. 128 is
`KEY_STOP`, standing in for CUE because Linux has no cue keycode; the button's
three behaviours are all userspace interpretation of one keycode.
`KEY_PREVIOUSSONG` (165) and `KEY_NEXTSONG` (163) exist if the two meanings are
ever split onto separate buttons. **Read all of these off `input-event-codes.h` on
the actual image** rather than trusting the numbers here — the same caution as the
overlay parameter names above.

Standard Linux input codes, so `/dev/input` events read as what they mean.
Decode encoders and debounce buttons **in the kernel**, never by polling from
userspace — polling quantises jog velocity to the poll interval and drops steps.

## Output

Use the **RCA coax** output, not TOSLink: the datasheet itself warns that not all
DACs manage 96/192 kHz optically. 75 ohm output impedance, so use 75 ohm cable.

The output is **transformer-coupled** — the datasheet lists an output isolation
transformer — which is good practice and is also what makes the isolation ground
jumper above meaningful.

There is also an **optional BNC connector**. Electrically it is the better choice:
BNC is a true 75 ohm connector where RCA is not impedance-matched at all. Whether
it is worth fitting depends entirely on the DAC's input, which is almost always
RCA.

Do not fit the Digi2 Pro's optional DSP — it would break bit-perfect. Do not fit
the IsolatorPi III's DoP decoder daughter board — the source is PCM, and it would
put DSD signals onto the non-isolated GPIO where the controls live.

Because the DoP board is not fitted, **leave J8 at its factory default** — the
manual (§J) says to touch J8 only when a DoP decoder is installed.

## Interfaces and limits

- S/PDIF: 44.1-192 kHz, 24 bit max. The WM8804 driver actually advertises 32 and
  64 kHz as well, and both are exact integer divisions of the 48 kHz crystal, but
  they are below the board's stated interface floor and out of scope regardless.
- The isolator IC is a **Chipanalog CA-IS376x** — the board photo reads
  `CA-IS3760HW` (exact digits worth re-checking on the board). This is the part
  needed to answer the power-on-order half of open question 6: what its outputs do
  when one side is unpowered is a datasheet fact, not something to reason about.
- Crystals are **22.5792 MHz** (44.1 family) and **24.576 MHz** (48 family). Not
  printed in the datasheet; derived from the driver's own arithmetic, which sets
  MCLK to `Fs x 128` above 96 kHz — exactly those two figures at 176.4 and 192 kHz.
- Output format is **S24_LE**; `S32_LE` is not available. See
  `implementation.md`.
- Isolator: 150 MHz, 768 kHz I2S / DSD512 — no bandwidth concern at any rate here
- Digi2 Pro has no volume or tone control by design; it passes what the
  application sends

## Sources

Primary documents for the two off-the-shelf boards. Every hardware fact above
should be traceable to one of these; where it is not, the text says so.

- **IsolatorPi III User's Guide** (Ian Canada, 2024) —
  <https://github.com/iancanada/DocumentDownload/tree/master/IsolatorPi>
  ([direct PDF](https://raw.githubusercontent.com/iancanada/DocumentDownload/master/IsolatorPi/IsolatorPiIIIUsersManual.pdf)).
  §E connectors and J6 pinout, §F jumpers (J12/J13 master-slave, J8 DoP), §H LEDs,
  §J application notes. Same directory has `isolatorpiiii.dxf`, the board outline —
  useful for the enclosure — and **`IsolatorPiIII.jpg`**, a 1620x1080 board photo
  readable enough to settle silkscreen questions. It is the source of J1's screw
  terminal, the `CA-IS376x` isolator marking, and the J12/J13 `SLAVE`/`MASTER`
  labels. Board is 65 x 65.5 mm.
- **Raspberry Pi 3 Model B+ product brief** (Raspberry Pi Ltd, published
  2025-10) —
  <https://datasheets.raspberrypi.com/rpi3/raspberry-pi-3-b-plus-product-brief.pdf>
  Specification and input-power table, operating temperature, case warnings.
- **Frequency management and thermal control** —
  <https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#frequency-management-and-thermal-control>
  and `temp_soft_limit` / `core_freq_fixed` under
  <https://www.raspberrypi.com/documentation/computers/config_txt.html#overclocking-options>.
  Source of the 60 C soft limit and the 4.8 V figure. Docs source is
  <https://github.com/raspberrypi/documentation> if the rendered pages are hard
  to quote.
- **HiFiBerry Digi2 Pro datasheet** (last updated 2022-10-17) —
  <https://www.hifiberry.com/docs/data-sheets/datasheet-digi2-pro/>
  · index: <https://www.hifiberry.com/docs/>
- **Pioneer CDJ-350 operating instructions** (389414-01U) —
  <https://imagescdn.juno.co.uk/manual/389414-01U.pdf>
  p.17-18 is the reference for CUE and for FF/REW: Setting Cue, Back Cue, Cue Point
  Sampler, auto cue, and the SEARCH / TRACK SEARCH split. The right comparison for a
  five-button single player, where a CDJ-3000X is not.
- **Digi2 Pro board photo** —
  <https://www.hifiberry.com/wp-content/uploads/2021/02/board-parts.jpg>
  1200x1200 and readable. Source of the `JP1` designator beside the output
  transformer, `P3` for the 5 V input, the `P4` BNC footprint, `U1` as the WM8804,
  and the board printing its own `dtoverlay=hifiberry-digi-pro` line. Marked
  "HW 2.1".
- **CA-IS376x datasheet** (Chipanalog) —
  <https://e.chipanalog.com/Public/Uploads/uploadfile/files/20240611/CAIS376xdatasheetVersion1.06en.pdf>
  Six-channel digital isolator. The `H`/`L` suffix sets the fail-safe output state
  when a side is unpowered, which is what settles power-on order.
- **GPIO usage of HiFiBerry boards** —
  <https://www.hifiberry.com/docs/hardware/gpio-usage-of-hifiberry-boards/>
  The authority for which pins the Digi2 Pro claims: GPIO 2/3, 5, 6 and **18-21**.
  Also the source of the 3.3 V current limit and the warning against adding I2C
  slaves. Read the per-board sections carefully — the Digi+ and the Digi+ Pro /
  Digi2 Pro reserve *different* pins, and the EEPROM line says "pins 27 and 28",
  meaning physical pins, not GPIOs.

Ian Canada's manuals are also mirrored on third-party manual-aggregator sites.
**Do not cite those** — one of them transcribes the J12/J13 table backwards.

### Unverified against the physical boards

One left, plus one that costs nothing either way.

**Resolved, from a legible scan of §F:** the J12/J13 values. The table agrees with
all six worked examples — master is J13 shorted, J12 open — so the manual is
self-consistent and the apparent contradiction was a bad reading of the silkscreen
from an oblique photo. See the assembly checklist.

1. **A pass-through GPIO header — reopened by the photo.** The datasheet's
   "Connectors and Jumpers" section enumerates DSP connector, 5 V power supply,
   TOSLink, RCA, isolation ground jumper and optional BNC, with no pass-through,
   which argued for a terminating HAT. But the board photo shows structure at the
   40-pin position on the *top* face that could be either solder tails or a stacking
   header. A side-on view or the board itself settles it.

   What is at stake is convenience, not capability. With a pass-through, phases A
   and B merge: no HAT swapping, and — more usefully — **browse and play can run
   together before the isolator arrives.** That is the one thing neither phase covers
   alone: the control-thread-to-callback path under real audio load, redraw timing
   while audio runs, and the display sharing I2C with the WM8804. That last one is
   currently deferred to C, and a pass-through would surface it earlier *and in a
   harsher form*, because without the isolator there is no bus split and the
   pull-ups really do parallel — HiFiBerry's own caution in its pure state. Finding
   that early is worth more than finding it tidily.

   Without one, nothing is blocked. Swap the HAT between A and B, or buy a GPIO
   splitter (another unsupported interposer, and the isolator is coming anyway), or
   let C do the integration. Not worth chasing — but if the header turns out to be
   there, use it.
3. **The Digi2 Pro's `JP1`, the "isolation ground jumper".** Two circumstantial
   lines make the identification safe: the datasheet's connector list carries an
   "isolation ground jumper", and the board photo shows `JP1` immediately beside the
   output isolation transformer. What it *does* is still undocumented.

   The transformer narrows it, though. Its marking reads **Pulse `T6074NL`**, which
   is the **electrostatically shielded** variant of a 1:1 digital-audio transformer —
   225 uH, 1500 Vrms. An electrostatic shield is a conductor between primary and
   secondary with its own terminal, and it does nothing at all unless grounded. So
   the likeliest function of a jumper sitting right next to it is:

   > **grounding the transformer's electrostatic shield, or not.**

   That would make it a noise-rejection option, not a grounding-topology one — and
   it would mean **open question 6's grounding question does not depend on `JP1`
   after all.** An earlier version of this file claimed it did, on the assumption
   that `JP1` bonded the *output connector's* ground and shield to board ground.
   That reading is not ruled out; the name reads both ways, since grounding a shield
   improves the isolation while bonding the output ground would defeat it.

   **A continuity check settles it — no vendor query needed.** With a meter on the
   bare board:

   | `JP1`'s pins connect to | Reading |
   |---|---|
   | the transformer's middle pin and board ground | grounds the electrostatic shield |
   | the **RCA shell** and board ground | bonds the output ground |

   The advice that the clean supply's secondary should float rests on the second
   case. So this belongs to open question 6, not to a list of loose ends.

**Resolved without the boards:** "does the Digi2 Pro actually drive GPIO 5/6?"
used to be a third item here. The overlay names the pins and the machine driver
switches them, so it was answered by reading `hifiberry-digi-pro-overlay.dts` and
`rpi-wm8804-soundcard.c` — see the assembly checklist and `implementation.md`.

Worth generalising: a question about a *driver's* behaviour is usually answerable
from kernel source right now, even when a question about the *board* needs the
board. The two boards' own jumpers and connectors are the genuinely physical
part.

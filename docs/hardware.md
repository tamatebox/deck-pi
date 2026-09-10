# Hardware

The boards, the pins, and what to check. `decisions.md` has why; the issues have
what is still open.

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

**Read §C of the manual for the board's own block diagram** — the MUX, the optional
DoP decoder, three separate isolators (I2S/DSD, Control I2C, GPIO) and the two power
domains. Better than anything redrawn from these sentences, and redrawing a circuit
in a software notation is how this file used to get it wrong.

What the manual does *not* do is map its pins onto the Pi's numbering, which is what
this file adds. §E's J6 table, transcribed:

| J6 pin | Signal | |
|---|---|---|
| 2, 4 | Clean side power supply | pass-through of J1; "entirely isolated from the RPi 5V" |
| 1, 17 | 3.3 V | isolated, regulated, 200 mA out |
| 3, 5 | I2CDA, I2CCL | isolated I2C, to configure the audio card |
| **12, 35, 40** | **SCK/BCK, LRCK/DL, DATA/DR** | isolated I2S — **three signals, not four** |
| 6, 9, 14, 20, 25, 30, 34, 39 | GND | isolated ground |
| 15, 22 | PULL UP | 4.7 k to 3.3 Vcc, for cards that need it |
| 29, 31 | XO selection | "isolated GPIO5 and GPIO6" |
| all others | **NC** | not connected |

Two things fall out of the last row. **Physical pin 38 — GPIO 20, PCM_DIN — is NC**,
so it never reaches the audio card; it stays reserved because the Pi's own I2S
interface claims it, which is a different connector's problem. And **J1 is wired
directly to J6 pins 2 and 4**, so the clean supply can be fed at either.

**J4 parallels the input connector pin-for-pin**, so the controls and display sit on
the same forty conductors, on the near side of the gap. §J-3 is the rule that
matters: *do not make any link between the input side and the output side.* Doing so
defeats the isolation and produces no other symptom.

The Digi2 Pro's bundled M2.5x12 mm spacers assume it mounts straight onto a Pi. The
isolator sits between them, so longer standoffs are needed.

## The Pi

Raspberry Pi 3 Model B+ — same header and footprint as a 3B, so nothing here depends
on which it is.

| | |
|---|---|
| SoC | Broadcom BCM2837B0, quad Cortex-A53, **1.4 GHz** |
| RAM | 1 GB |
| Ethernet | Gigabit, **over USB 2.0** — 300 Mbps maximum |
| Wireless | 802.11b/g/n/ac dual-band 2.4/5 GHz, BT 4.2/BLE — both disabled here |
| USB | 4 x USB 2.0 on one shared bus |
| Power in | 5 V / 2.5 A micro-USB, **or 5 V via the GPIO header**; PoE needs a separate HAT |
| Operating temp | 0-50 C |

Supply voltage must stay **above 4.8 V**; some USB supplies sag to 4.2 V, being built
to charge a LiPo rather than run a computer. `vcgencmd get_throttled` reports
undervoltage, but only after it has happened.

### 1.2 GHz is the planning number, not 1.4

The 3B+ has a **soft** temperature limit — 60 C by default — that drops the clock
from 1.4 to 1.2 GHz to trade a short sprint for a longer run. `temp_soft_limit`
raises it to 70 at most, and the docs say that "might cause instability".

A deck runs a continuous realtime load, in a box, for the length of a set, so
**1.2 GHz is the steady state** and what `architecture.md` sizes against. Going 3B to
3B+ buys thermal mass and Gigabit Ethernet; it does *not* buy 17% of resampler
headroom. It also settles how the v2 libsoxr benchmark must run: **thermally soaked**,
or it reports a clock the board will not hold and passes a part that fails twenty
minutes into a set.

Official case guidance is that a case "should not be covered", which a sealed DJ
enclosure is in tension with — [#6](https://github.com/tamatebox/deck-pi/issues/6).

## Assembly checklist

Three things are easy to get wrong and produce no error when wrong.

**1. Set J12 / J13 to master mode.** The default is *slave*, in which the Pi
generates the I2S clock from its own PLL — the high-jitter path this build exists to
avoid. **Audio plays either way**, so nothing surfaces the mistake.

   | | J13 | J12 |
   |---|---|---|
   | Slave (default) | open | 1-2 and 3-4 shorted |
   | **Master — use this** | **1-2 and 3-4 shorted** | **open** |

§F's table and all six of §I's worked examples agree, so the manual is
self-consistent; example 1 is the directly applicable one. In master mode the Digi2
Pro's two crystals generate SCK and LRCK and feed them back, and the Pi generates
only DATA — which is why the jumpers exist at all: the isolator's channels are
one-directional and have to be told which way the clocks flow. §E lists exactly
**three** I2S pins crossing J6 — 12 SCK, 35 LRCK, 40 DATA — pin 38 being NC.

**Both jumpers go on vertically.** Pins are 3 and 1 across the top, 4 and 2 across
the bottom, so each shunt bridges one column top-to-bottom. Placing them horizontally
is the wrong orientation §J-4 warns can *damage* the board — check before applying
power.

**2. Feed J1 with clean 5 V, and never power the isolated side from the Pi.** J1
regulates the isolator and passes the supply through to J6 pins 2/4. Powering from
both sides bridges the isolation and defeats the build; the Digi2 Pro's own 5 V
connector goes unused. J1's stated range is 3.3-5 V and Ian's examples feed 3.3 V,
but those drive 3.3 V DACs — here the same rail reaches a 5 V HAT. It is a green
2-pin screw terminal marked `CLEAN POWER` with `⊕` for positive.

**3. Leave GPIO 5 and 6 free.** J6 pins 29/31 carry them through as the
oscillator-select lines; the overlay names them `clock44-gpio` and `clock48-gpio`,
and the machine driver switches them on every rate change. **The failure mode is
worse than "oscillator select stops working":** if the driver cannot get those GPIOs
it falls back to `sysclk_freq = 27000000`, which is the WM8804's **PLL** reference —
so audio still plays, through the jitter path the build exists to avoid.

## Bring-up order

Fit the isolator **last**: §J-1 says to prove the hardware and software produce audio
*before* inserting it, because debugging is much harder afterwards. The audio half and
the control half are independent and neither waits for the other; the only constraint
is that the Digi2 Pro is a terminating HAT, so swap it on and off.

- **A — bare Pi, no audio hardware.** Controls straight to the header, a panel on
  SPI0, a stick in a USB port. Everything except the audio engine, and it settles
  [#2](https://github.com/tamatebox/deck-pi/issues/2) and
  [#4](https://github.com/tamatebox/deck-pi/issues/4). The null test needs no audio
  hardware either.
- **B — Pi plus Digi2 Pro, audio only.** Bundled spacers are right and the overlay is
  already explicit, so `config.txt` does not change later. Mostly one command:
  `deck-pi --device=hw:X,Y <file>` opens at the track's own rate, prints the period
  geometry ALSA granted, checks the mixer is empty, plays through the whole chain and
  reads `hw_params` back. `plughw:` is refused before ALSA is touched.
- **C — Pi, isolator, Digi2 Pro.** Integration: longer standoffs, J12/J13, clean 5 V
  on J1, the grounding question, and the controls moving to J4.

Controls belong in C rather than earlier — reaching the header past the HAT needs a
splitter and then rewiring onto J4 — and §J-1 is about *audio*, not controls. **The
integration risk that used to sit here is gone**: it was the display sharing I2C with
the WM8804, and the panel is on SPI now, so A's redraw timing carries to C. The
libsoxr benchmark ([#3](https://github.com/tamatebox/deck-pi/issues/3)) needs none of
the three.

Two things that do *not* change between B and C: **GPIO 5 and 6 stay reserved**, being
Pi pins routed *through* the isolator, so removing it does not free them; and **the
Digi2 Pro's crystals are the clock master either way** — what the isolator adds is
ground and power separation, not clock purity, which §J-2 is blunt about. So A and B
together are the whole v1 software stack, but B is **not** an audio-quality baseline.

## Power budget (clean side)

| | Draw | Provenance |
|---|---|---|
| IsolatorPi III | ~100 mA | manual §E — but see below |
| Digi2 Pro | <0.3 W (~60 mA) | datasheet |
| **Total** | **under 200 mA at 5 V** | neither figure is measured here |

That 100 mA is the manual's only current figure and it is quoted **with the DoP
daughter board fitted**, which this build does not fit — so the budget errs safe but
is not a measurement of this configuration. J6 also offers an isolated regulated
**3.3 V / 200 mA** on pins 1/17 and 4.7 k pull-ups on 15/22; neither is used here.
The isolator's own **D1** lights when the Pi side has power and **D3** when clean
power is present, which is a free answer to "is the clean side up?". 0.5 A is ample,
and both boards regulate their own input, so a plain linear supply is enough.

## The header

**The physical header, as it is laid out** — odd pins down the left, even down the
right, the format pinout.xyz established. This is the **anti-miscount artifact**: the
tables below say what each GPIO is *for*, and only this one says what is *next to*
what.

| Use | GPIO | odd | even | GPIO | Use |
|---|---|---:|:---|---|---|
| — | 3V3 | 1 | 2 | 5V | — |
| **I2C SDA** | **2** | 3 | 4 | 5V | — |
| **I2C SCL** | **3** | 5 | 6 | GND | — |
| held — source | 4 | 7 | 8 | **14** | **UART TXD** |
| — | GND | 9 | 10 | **15** | **UART RXD** |
| encoder A | 17 | 11 | 12 | **18** | **I2S** |
| encoder B | 27 | 13 | 14 | GND | — |
| ENTER | 22 | 15 | 16 | 23 | BACK |
| — | 3V3 | 17 | 18 | 24 | PLAY / PAUSE |
| SPI0 MOSI | 10 | 19 | 20 | GND | — |
| panel DC *(MISO)* | 9 | 21 | 22 | 25 | CUE |
| SPI0 SCLK | 11 | 23 | 24 | 8 | SPI0 CE0 |
| — | GND | 25 | 26 | 7 | *unity (v2)* |
| **ID EEPROM** | **0** | 27 | 28 | **1** | **ID EEPROM** |
| **clock44** | **5** | 29 | 30 | GND | — |
| **clock48** | **6** | 31 | 32 | 12 | *jog A (v2)* |
| *jog B (v2)* | 13 | 33 | 34 | GND | — |
| **I2S** | **19** | 35 | 36 | 16 | REW |
| FF | 26 | 37 | 38 | **20** | **I2S — PCM_DIN** |
| — | GND | 39 | 40 | **21** | **I2S** |

**bold** — hard-reserved. *italic* — v2. Ground is 6, 9, 14, 20, 25, 30, 34, 39, all
interchangeable.

Two things this makes visible that a table sorted by GPIO cannot. **GPIO 20 sits at
physical pin 38**, between FF and the I2S pin at 40 — surrounded by the interface that
claims it, which is what the flat table failed to show when a button was assigned
there. And **the I2S four are 12, 35, 38, 40**: one is nowhere near the others, so
"the I2S block" is not a region you can avoid by staying away from one end.

Where this table and the two below disagree, one of them is wrong and it must be
resolved, not averaged.

**Reserved — 12 pins**

| GPIO | Use |
|---|---|
| 0, 1 | HAT ID EEPROM (physical pins 27/28 — *pins*, not GPIOs, a documented trap). Also **I2C0**, a second controller — see below |
| 2, 3 | I2C — WM8804 control, and the v2 ADC |
| 5 | 44.1 kHz crystal enable (`clock44-gpio`) |
| 6 | 48 kHz crystal enable (`clock48-gpio`) |
| 14, 15 | Serial console (PL011, freed by `disable-bt`) |
| **18, 19, 20, 21** | I2S — **four** pins |

**GPIO 20 is I2S, not spare.** HiFiBerry reserves 18-21 for the sound interface on
the Digi2 Pro and says they cannot be used for anything else. An earlier version of
this table left 20 free and a button was assigned to it. GPIO 16 *is* free: HiFiBerry
reserves it on the plain Digi+, but the Digi2 Pro entry replaces that with 5 and 6.

**SPI0 (7, 8, 9, 10, 11) carries the display panel.** Two things wanted this block —
the v2 fader's ADC and an SPI panel — and exactly one could be made to want I2C
instead. The ADC went there. The panel takes four: SCLK, MOSI, CE0, and **GPIO 9 as
DC**, a write-only panel having no use for MISO. Neither GPIO 9 nor unity's GPIO 7 is
free by default — plain SPI0 claims all five — and one overlay line releases both:
`dtoverlay=spi0-1cs,no_miso`, whose `cs1_pin` defaults to 7 and whose `no_miso`
"don't claim and use the MISO pin (9)". Read from the overlay README. **`config.txt`
carries no SPI line at all yet.**

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
| 7 | **Unity** — passthrough on/off | v2 |
| 4 | **Held** for a source toggle — this deck's USB, or a peer deck's over the network | not scheduled |

**This table is the assignment, and two other places follow it.**
`implementation.md`'s `gpio-key` lines and `src/input.rs`'s keycodes repeat it. The
Rust pair is guarded by a bijection test; nothing guards either against `config.txt`.
Said plainly because this section already records two pins assigned wrongly for want
of a table showing what another table showed.

**Seven switches, and the last pin is held rather than free** — BACK, PLAY/PAUSE, CUE,
FF, REW, the encoder's push as ENTER, and unity in v2. GPIO 4 is held for a source
toggle, raised tentatively rather than decided. Read that as "no room", not "one
spare": the panel's four SPI pins have taken everything else. Unity takes **GPIO 7**,
SPI0's second chip select — one ADC needs one, not two.
**[#4](https://github.com/tamatebox/deck-pi/issues/4) is a swap, not an addition**,
and with nothing spare a swap is the only shape of change the header can absorb.

### What the header has left

Subtract from 28: ten hard-reserved (EEPROM, I2C, the two crystal selects, I2S), two
for the serial console, four for the encoders, four for the panel. That leaves
**eight** — seven used, the eighth held.

**Freeing SPI0 is not the same as leaving it empty.** A write-only panel takes SCLK,
MOSI, CS and DC, the same four an SPI ADC would have taken, which is why the figure is
eight and not twelve. `decisions.md` records the version of this arithmetic that spent
the same saving twice, and
[#1](https://github.com/tamatebox/deck-pi/issues/1) the branches not taken.

**The tally balancing at zero is the finding, not the ceiling.** It promotes two
properties of a panel nobody has bought to load-bearing, both in
[#2](https://github.com/tamatebox/deck-pi/issues/2): that **RESET is tied high** and
takes no GPIO, and that the **backlight needs no PWM pin**. Either one costs a
control. Past that, the routes are the serial console's two pins, the v2 jog's two, an
I2C port expander, or dropping something.

Three cautions from HiFiBerry's GPIO-usage page, all of which this build touches:

- **"Do not use more than a few mA from the 3.3V line."** They ask for 5 V plus a
  regulator. A small OLED at 10-25 mA is already past "a few" and a backlit TFT far
  past it, so the display gets 5 V and its own regulation.
- **They do not recommend adding I2C slaves** alongside the WM8804, their reason being
  uncertain pull-ups. The isolator largely answers it: a Control I2C Isolator sits
  between the two sides and J6 carries dedicated pull-up pins, so I2C is **two
  electrically separate segments** — read from the block diagram rather than stated,
  so worth a scope on the real stack. What does not go away is **bus time**, one
  logical bus from one controller; the v2 ADC is cheap there, a two-byte read at
  100 Hz being about 1% of a 400 kHz bus. That ceiling is the codec's: the WM8804
  datasheet (v4.5, Table 5) caps SCLK at **400 kHz**. The BCM2837 does have a second
  controller, I2C0 on GPIO 0/1, but three things about using it are unknown and all
  silent when wrong — see [#2](https://github.com/tamatebox/deck-pi/issues/2).
- **The whole stack is outside HiFiBerry's supported configuration.** No guarantee of
  interoperability with other cards, and the isolator is an interposer. Ian Canada's
  manual supports the Digi Pro in master mode explicitly, so the combination is sound
  — but neither vendor supports it.

## Where J4 is

`J4` is a reference designator silkscreened on the IsolatorPi III; the numbers are not
sequential by position. The board carries three 40-pin connectors:

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

J4 and J6 are upward-facing male headers **side by side**, not stacked, so whatever
connects to J4 needs a female socket. The isolator is **65.5 mm** deep against a
standard HAT's 56 mm and J4 sits at the outer edge, so it should stay reachable with
the stack assembled — deduced from the dimensions and the photo, so confirm on the
boards. Anything hung there is on the near side of the gap, which is why the display
needs no noise mitigation of its own; the manual names rotary encoders as an intended
use.

## Controls

**v1** — one detented rotary encoder (EC11 class; the clicks are an asset for menu
stepping) plus five buttons: BACK, PLAY/PAUSE, CUE, FF, REW. ENTER is the encoder's
own push. **What each one does is `controls.md`;** this section is how many, of what
kind, and wired how.

**v2** — a non-detented *optical* encoder for the jog, detents being disqualifying
because the notches are felt through the platter while scrubbing. 100-200 PPR
(400-800 counts/rev after x4 decoding) is enough with no scratching; below ~400,
low-speed velocity estimation breaks down. Plus a pitch fader, which needs an ADC —
**on I2C**, which is what left SPI0 for the panel.

### How many of what

Counts and kinds, deliberately without part numbers: **a part you own is a fact; a
part you might buy is a constraint.** `WM8804` and `CA-IS376x` are named throughout
this file because they are on the bench; nothing below is.

| | n | GPIO | What the kind has to be |
|---|---|---|---|
| Browse encoder, with push switch | 1 | 17, 27, 22 | Detented. The clicks step the menu, and the push is ENTER |
| PLAY / PAUSE | 1 | 24 | Real travel. Takes the most abuse of anything here |
| CUE | 1 | 25 | Real travel |
| BACK | 1 | 23 | A small tactile is fine |
| FF | 1 | 26 | Comfortable **held** for seconds, not crisp |
| REW | 1 | 16 | Same |
| UNITY — v2 | 1 | 7 | Set apart from PLAY, and different to the finger. A mis-press changes the audio path and the handover is cross-faded, so **it makes no sound** |
| Source toggle — USB or a peer deck | **0 or 1** | 4 | Held, not scheduled. The only control asked for beyond the set above, and asked for tentatively |
| Jog encoder — v2 | 1 | 12, 13 | **Non**-detented, optical, 100-200 PPR |
| Pitch fader — v2 | 1 | via the ADC | Linear taper, and a **centre detent**: it is what makes UNITY's "near centre" gate a physical fact rather than an inference. Detented slide pots run out around 60 mm of travel, which at ±10% is 3 mm per 1% and ample |
| Display panel | 1 | 11, 10, 8, 9 | SPI (9 is DC). **RESET tied high and no PWM backlight** — both load-bearing, see *What the header has left* |
| ADC | 1 | on I2C | 16-bit is available for nothing and costs no pins |

**Seven switches counting the encoder's push. Two encoders. One fader.** That is the
whole control surface, and the pin tally balances at zero spare.

Also needed, none of it a control: a 5 V regulator for the panel, a 40-pin IDC ribbon
and a screw-terminal breakout for J4, four standoffs longer than the bundled
M2.5x12 mm, a clean linear 5 V supply for J1, an enclosure, and stranded 26-28 AWG
wire wherever it flexes.

**One temperature constraint crosses all of it**, and it belongs to the enclosure
rather than to any part: commodity slide potentiometers are commonly rated to
**+50 °C** while the Pi's soft limit is **60 °C**, so an enclosure hot enough to
throttle the Pi is already outside a panel component's rating. A floor on the box,
not a spec of a part — [#6](https://github.com/tamatebox/deck-pi/issues/6).

**Cheap encoder push switches bounce badly and wear out, and ENTER is the most-used
control** — the substance of [#4](https://github.com/tamatebox/deck-pi/issues/4):
raise the debounce interval to 30-50 ms, or give ENTER its own button and free
GPIO 22.

### Wiring them

Two wires per button, one terminal to its GPIO and the other to any ground pin.
Grounds can be shared, so v1 is **nine signal lines plus a ground**. Buttons pull to
ground and use the internal pull-ups — no external resistors.

**The numbering is the trap.** GPIO number is not physical pin number, and the pins
alternate odd and even across the two rows. **Take the pin numbers from the header map
above**, which is deliberately the only copy.

Verify rather than assume: **`evtest`** prints `/dev/input` events, so a press either
produces the expected keycode or it does not, and **`pinout`** (ships with gpiozero)
prints the running board's own header as ASCII — the table above is a document, that
is the machine.

**Phase A** wants a labelled GPIO breakout to a breadboard; the printed names are what
stop the miscount. **Phase C** wants a **screw-terminal breakout** on a 40-pin IDC
ribbon, mounted away from J4 rather than stacked on it — the same absence of a latch
that makes a knocked USB connector plausible applies to internal wiring. Only about
ten of the forty pins are used. Use **stranded** wire anywhere it flexes; solid wire
work-hardens and breaks.

## Output

Use the **RCA coax** output, not TOSLink: the datasheet warns that not all DACs manage
96/192 kHz optically. 75 ohm out, so use 75 ohm cable. The output is
**transformer-coupled**, which is what makes the isolation ground jumper meaningful.
An optional **BNC** is electrically better — a true 75 ohm connector where RCA is not
impedance-matched at all — but depends on the DAC's input, which is almost always RCA.

Do not fit the Digi2 Pro's optional DSP: it would break bit-perfect. Do not fit the
isolator's DoP decoder: the source is PCM, and it would put DSD onto the non-isolated
GPIO where the controls live. Because it is not fitted, **leave J8 at its factory
default** — §J says to touch J8 only when a DoP decoder is installed.

## Interfaces and limits

- S/PDIF: **44.1-192 kHz, 24 bit max.** The driver also advertises 32 and 64 kHz, both
  exact divisions of the 48 kHz crystal, but they are below the board's stated floor
  and out of scope regardless. Output format is **S24_LE**; `S32_LE` is not available.
- Crystals are **22.5792 MHz** and **24.576 MHz** — not printed in the datasheet,
  derived from the driver setting MCLK to `Fs x 128` above 96 kHz.
- The isolator IC is a **Chipanalog CA-IS376x** (photo reads `CA-IS3760HW`, worth
  re-checking on the board). Its `H`/`L` suffix sets the fail-safe output state when
  one side is unpowered, which is what settles power-on order —
  [#7](https://github.com/tamatebox/deck-pi/issues/7).
- Isolator bandwidth is 150 MHz / 768 kHz I2S, so no concern at any rate here. The
  Digi2 Pro has no volume or tone control by design.

## Unverified against the physical boards

**A pass-through GPIO header** —
[#13](https://github.com/tamatebox/deck-pi/issues/13). The datasheet's connector list
has none, which argued for a terminating HAT, but the board photo shows structure at
the 40-pin position on the *top* face that could be solder tails or a stacking header.
A side-on view settles it. What is at stake is convenience, not capability: with one,
phases A and B merge and **browse and play can run together before the isolator
arrives** — the control-thread-to-callback path under real audio load, and redraw
timing while audio runs. Without one, nothing is blocked.

**The Digi2 Pro's `JP1`** —
[#5](https://github.com/tamatebox/deck-pi/issues/5). The datasheet lists an "isolation
ground jumper" and the photo shows `JP1` beside the output transformer, so the
identification is safe; what it *does* is undocumented. The transformer narrows it:
its marking reads **Pulse `T6074NL`**, the **electrostatically shielded** variant, and
a shield does nothing unless grounded — so the likeliest function is grounding the
shield or not, making it a noise option rather than a grounding-topology one. That
reading is not certain; the name reads both ways. **A continuity check settles it, no
vendor query needed:**

   | `JP1`'s pins connect to | Reading |
   |---|---|
   | the transformer's middle pin and board ground | grounds the electrostatic shield |
   | the **RCA shell** and board ground | bonds the output ground |

The advice that the clean supply's secondary should float rests on the second case, so
this belongs to [#7](https://github.com/tamatebox/deck-pi/issues/7).

Two resolved without the boards, kept because both were once listed here. The J12/J13
values came from a legible scan of §F, the earlier refusal having rested on an oblique
photo of the silkscreen. And "does the Digi2 Pro actually drive GPIO 5/6?" was answered
by reading `hifiberry-digi-pro-overlay.dts` and `rpi-wm8804-soundcard.c`. **Worth
generalising:** a question about a *driver's* behaviour is usually answerable from
kernel source right now, even when a question about the *board* needs the board.

## Sources

Every hardware fact above should be traceable to one of these; where it is not, the
text says so.

- **IsolatorPi III User's Guide** (Ian Canada, 2024) —
  <https://github.com/iancanada/DocumentDownload/tree/master/IsolatorPi>
  ([PDF](https://raw.githubusercontent.com/iancanada/DocumentDownload/master/IsolatorPi/IsolatorPiIIIUsersManual.pdf)).
  §E connectors and J6, §F jumpers, §H LEDs, §J application notes. Same directory has
  `isolatorpiiii.dxf` (board outline, useful for the enclosure) and
  **`IsolatorPiIII.jpg`**, a 1620x1080 photo readable enough to settle silkscreen
  questions — the source of J1's screw terminal, the `CA-IS376x` marking and the
  `SLAVE`/`MASTER` labels. Board is 65 x 65.5 mm. **Ian's manuals are mirrored on
  manual-aggregator sites; do not cite those** — one transcribes the J12/J13 table
  backwards.
- **Raspberry Pi 3B+ product brief** —
  <https://datasheets.raspberrypi.com/rpi3/raspberry-pi-3-b-plus-product-brief.pdf>
  Specification, input-power table, operating temperature, case warnings.
- **Frequency management and thermal control** —
  <https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#frequency-management-and-thermal-control>
  with `temp_soft_limit` under
  <https://www.raspberrypi.com/documentation/computers/config_txt.html#overclocking-options>.
  Source of the 60 C soft limit and the 4.8 V figure.
- **HiFiBerry Digi2 Pro datasheet** —
  <https://www.hifiberry.com/docs/data-sheets/datasheet-digi2-pro/>
- **GPIO usage of HiFiBerry boards** —
  <https://www.hifiberry.com/docs/hardware/gpio-usage-of-hifiberry-boards/>
  The authority for which pins the Digi2 Pro claims (2/3, 5, 6, **18-21**), the 3.3 V
  current limit and the I2C-slave warning. Read the per-board sections carefully — the
  Digi+ and the Digi2 Pro reserve *different* pins, and "pins 27 and 28" means
  physical pins.
- **Digi2 Pro board photo** —
  <https://www.hifiberry.com/wp-content/uploads/2021/02/board-parts.jpg>
  Source of `JP1` beside the output transformer, `P3` for 5 V in, the `P4` BNC
  footprint, and the board printing its own overlay line. Marked "HW 2.1".
- **WM8804 datasheet** (v4.5) — <https://statics.cirrus.com/pubs/proDatasheet/WM8804_v4.5.pdf>
  Table 5 is the authority for the **400 kHz** I2C ceiling (`tSCY` min 2500 ns, which
  is exactly 1/400 kHz, so the table is self-consistent). Cirrus's own CDN.
- **CA-IS376x datasheet** (Chipanalog) —
  <https://e.chipanalog.com/Public/Uploads/uploadfile/files/20240611/CAIS376xdatasheetVersion1.06en.pdf>
- **Device Tree overlay README** (`raspberrypi/linux`, `rpi-6.12.y`) —
  <https://raw.githubusercontent.com/raspberrypi/linux/rpi-6.12.y/arch/arm/boot/dts/overlays/README>
  What each overlay claims and what its parameters release. Source of `spi0-1cs`'s
  `cs1_pin` default of GPIO 7 and `no_miso` freeing GPIO 9 — the two pins the
  zero-spare tally depends on. Also where to check `gpio-key` and `rotary-encoder`
  parameters before writing them into `config.txt`.
- **Raspberry Pi GPIO pinout** — <https://pinout.xyz/> — the header map's layout and
  the two-column convention. Common to every 40-pin Pi, but a *recalled-shaped* fact,
  so **confirm with `pinout` on the board** rather than against this table.
- **Pioneer CDJ-350 operating instructions** (389414-01U) —
  <https://imagescdn.juno.co.uk/manual/389414-01U.pdf> — p.17-18 for CUE and FF/REW.
  Used by `controls.md`.
- **Bourns PTA series datasheet** — <https://www.bourns.com/docs/product-datasheets/pta.pdf>
  Cited for a *market* fact, not a part: the centre detent is one digit of the
  ordering code, and travel tops out at 60 mm.

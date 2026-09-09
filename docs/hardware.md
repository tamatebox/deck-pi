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

   **The jumper values are deliberately not written down here.** The manual's
   table (§F) is a picture, not text. Its six worked examples agree with each
   other and imply master = J13 shorted 1-2 and 3-4, J12 all open — but the board
   silkscreen (§D) reads as SLAVE next to J13 and MASTER next to J12, and a
   third-party mirror of the same manual transcribes the table the other way
   round. Read the values off §F and off the board. The Digi2 Pro is WM8804-based,
   so application example 1 — "any WM8804/5 based Transport/DAC" — is the one
   that applies.

   A second failure mode sits on top of the silent one: per §J, a jumper in the
   wrong **orientation** can *damage* the board. Check before applying power.

2. **Feed J1 with clean 5 V, and never power the isolated side from the Pi.**
   J1 is the clean-side input; it regulates the isolator and passes the supply
   through to the audio board on J6 pins 2 and 4. Powering from both sides bridges
   the isolation and defeats the build. The Digi2 Pro's own 5 V connector is then
   unused.

   J1's stated range is **3.3-5 V**, and Ian's own application examples feed it
   3.3 V — but those drive DACs that run on 3.3 V. Here it must be 5 V, because
   the same rail passes straight through J6 pins 2/4 to a Pi HAT that expects
   5 V.

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

1. **Digi2 Pro direct onto the Pi, audio only.** The bundled M2.5x12 mm spacers
   are the right length for this, so nothing extra is needed.
   `dtoverlay=hifiberry-digi-pro` is already explicit, so `config.txt` does not
   change later. Confirm S/PDIF out at every rate, `hw:` device, and the
   byte-equality null test. **No controls at this stage** — the Digi2 Pro is a
   terminating HAT, so it occupies the whole 40-pin header and there is nowhere to
   put them.
2. **Insert the IsolatorPi III.** Longer standoffs, J12/J13, clean 5 V on J1, and
   the power topology all arrive here, and only here.
3. **Add the controls, on the isolator's J4.** Their final home, wired once.

The order of 2 and 3 matters, and an earlier version of this list had them
reversed. Controls before the isolator would have meant buying a GPIO splitter to
reach the header past the HAT, and then rewiring everything onto J4 afterwards.
The isolator manual's insistence on validating first (§J-1) is about **audio**; it
says nothing about controls, so nothing requires them to come earlier. This way
costs one fewer part and one fewer round of wiring.

Two things do *not* change between step 1 and step 3, and are easy to get wrong
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

So step 1 is the whole v1 software stack, but it is **not** an audio-quality
baseline. Anything measured or listened to there does not carry to step 3.

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

**Reserved — 11 pins**

| GPIO | Use |
|---|---|
| 0, 1 | HAT ID EEPROM |
| 2, 3 | I2C — WM8804 control, and the display |
| 5 | 44.1 kHz crystal enable (`clock44-gpio`) |
| 6 | 48 kHz crystal enable (`clock48-gpio`) |
| 14, 15 | Serial console (PL011, freed by `disable-bt`) |
| 18, 19, 21 | I2S |

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
| 25 | CUE / STOP | v1 |
| 16 | REW — hold to seek back, tap for previous | v1 |
| 20 | FF — hold to seek forward, tap for next | v1 |
| 12, 13 | Jog encoder A / B | v2 |
| 4, 26 | spare | |

Two spares remain, which is enough to cover open question 3 (a dedicated ENTER)
and still leave one.

Control peripherals connect to the IsolatorPi III's **J4**, the non-isolated 40-pin
passthrough. The manual names rotary encoders as an intended use. Anything hung
there is on the near side of the isolation gap, so its noise never reaches the
audio boards — which is why the display needs no noise mitigation of its own.

## Controls

**v1** — one detented rotary encoder (EC11 class; the clicks are an asset for
menu stepping) plus five buttons: BACK, PLAY/PAUSE, CUE/STOP, FF, REW. ENTER is
the encoder's own push switch.

Cheap encoder push switches bounce badly and wear out, and ENTER is the most-used
control. Either raise the debounce interval to 30-50 ms or give ENTER its own
button — the pin budget allows it. PLAY takes the most abuse and deserves a
switch with real travel; BACK can be a small tactile. FF and REW get held down for
seconds at a time, so pick switches that are comfortable to hold rather than
crisp.

Buttons pull to ground and use the internal pull-ups. No external resistors.

### FF and REW

Hold to seek, tap to change track. This is what makes long tracks usable: without
it the only entry point into an 80-minute piece is the beginning, since the jog is
v2.

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

Open: what a tap does at a folder boundary (stopping is the simple answer), and
whether a track auto-advances when it ends. Stopping is believed to be the usual
default on DJ players, but that is recollection, not a checked fact.

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
dtoverlay=gpio-key,gpio=25,keycode=128,label=STOP
dtoverlay=gpio-key,gpio=22,keycode=28,label=ENTER
dtoverlay=gpio-key,gpio=16,keycode=168,label=REW
dtoverlay=gpio-key,gpio=20,keycode=208,label=FF
```

168 and 208 are `KEY_REWIND` and `KEY_FASTFORWARD` — the *held* meaning, since one
pin carries one keycode and the tap meaning is a userspace interpretation.
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
  useful for the enclosure. Board is 65 x 65.5 mm.
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

Ian Canada's manuals are also mirrored on third-party manual-aggregator sites.
**Do not cite those** — one of them transcribes the J12/J13 table backwards.

### Unverified against the physical boards

Neither manual answers these outright. The first is now close to settled by
inference; the second is the one that still needs the board.

1. **A pass-through GPIO header — almost certainly absent.** The datasheet's
   "Connectors and Jumpers" section *enumerates* the board's connectors: DSP
   connector, 5 V power supply connector, TOSLink, RCA, isolation ground jumper,
   optional BNC. No GPIO pass-through appears, and it would be a selling point if
   it existed, so treat this as a terminating HAT and confirm by eye. This is why
   bring-up fits the isolator before wiring the controls: with a terminating HAT and
   no isolator, there is no header left to reach, and J4 does not exist yet. Fitting
   the isolator first removes the need for a GPIO splitter entirely.
2. **The Digi2 Pro's "isolation ground jumper" — the one that matters most.** The
   datasheet lists it and describes it nowhere. But the same datasheet also lists an
   **output isolation transformer**, and putting those together narrows it: on a
   transformer-coupled S/PDIF output, a jumper of that name most plausibly selects
   whether the output connector's ground and shield are bonded to board ground or
   left floating. That is the standard arrangement.

   **This is inference from the name and the topology, not documentation.** It needs
   the board in hand or an answer from HiFiBerry. It is worth chasing because it
   decides something already reasoned about in the power discussion: whether the
   clean side takes its ground reference through the coax shield from the DAC, or
   floats entirely on its own supply.

   | Jumper | Clean-side ground reference |
   |---|---|
   | open | none from the DAC — the clean side floats on its own supply |
   | closed | tied to the DAC's ground through the coax shield |

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

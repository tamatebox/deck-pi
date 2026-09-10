# Controls — what each one means

`hardware.md` says which switch sits on which pin. This file says what pressing it
*does*, and `decisions.md` carries the one-line rulings.

**The reference is the Pioneer CDJ-350 operating instructions (389414-01U, p.17-18),
read rather than recalled** — an entry-level single player, closer in scope to a
handful of buttons than a CDJ-3000X. That is a similarity of *button count*, not of
function. **It is one reference and not an authority**: the deck has no CD, no
network, no track database and no waveform. Every departure below is marked, because
a rule re-derived from memory is how "pausing at the head of the next track is the
CDJ's own behaviour" got written into two files while the manual said the opposite.

## CUE

| State | Tap CUE | The manual's name |
|---|---|---|
| Paused | **sets** the cue point at the paused position | Setting Cue |
| Playing | **returns** to the cue point and pauses there | Back Cue |
| Held at the cue point | **plays while held** | Cue Point Sampler |

Four details, quoted from that page:

- **One cue point per track** — "when a new cue point is set, the previously set cue
  point is canceled". A single point, not a set of hot cues.
- **Setting it makes no sound** — "no sound is output at this time".
- **Back Cue pauses; it does not resume** — "the set immediately returns to the
  currently set cue point and pauses". Playback restarts only on PLAY, from the cue.
- **The preview is momentary** — "playback continues while the button is held in", so
  release means stop and return. No latching.

**There is no separate STOP, because a CDJ has none.** Returning to the cue point and
standing by *is* stopping, which is why the button reads "CUE / STOP" and is one
function. So the hold gesture is free for preview instead of being spent on a stop
the transport already has. No new mechanism: hold is `r = 1.0`, release is `r = 0`
with the position set back.

**Departure — CUE during a held FF or REW is Back Cue.** The manual does not cover
the combination, so this is a chosen interpretation: anything not paused counts as
moving, and returning to the point is the predictable answer. Ignoring the press
would be worse, a control that sometimes does nothing being harder to trust than one
that always does the same thing.

**Departure — auto cue is not adopted.** The 350 has it: on load it skips the silent
lead-in and places the cue where sound starts, thresholds from -36 to -78 dB. One
piece opening below the threshold on purpose is enough to make that wrong, and a rule
must hold for everything on the stick. The cue starts at frame zero unless set.

Fine-adjusting the cue in single frames, which the 350 does with SEARCH while paused
at the cue, would fall naturally to FF/REW in the same state. Free if ever wanted.

## FF and REW

Hold to seek, tap to change track. **A tap loads the next track and waits at its
head — it does not start playing, even if the deck was playing.**

**Departure, and an earlier version of this paragraph claimed the opposite from
memory.** Read off the manual: TRACK SEARCH keeps playing, and pausing at the start
happens **only with auto cue on** — p.17, "when auto cue is turned on, the set
searches for the beginning of the track and pauses there". Selecting a track with the
rotary selector is more emphatic: "the track is loaded and playback begins."

Taking the pause anyway **separates the two halves of auto cue** rather than adopting
it. What is rejected is auto cue deciding *where the music begins*; pausing on
arrival decides nothing about the audio. The rule that buys is one line for the whole
panel: **nothing produces sound that the operator did not press PLAY for.** It also
means a track change always contains a pause, which the software design leans on —
`architecture.md`.

**This compresses two of the 350's controls into one pair, deliberately.** That
player separates SEARCH (`◄◄ ►►`, within a track) from TRACK SEARCH (`|◄◄ ►►|`,
between tracks) — four buttons where this deck has two. Worth knowing it is a
compression rather than the idiom. Each half earns its keep on different material:
hold-to-seek is what makes an 80-minute piece usable at all with no jog until v2,
tap-for-next carries the weight across a folder of short tracks.

It also removes a worse idea — overloading the browse encoder, browsing in the list
and seeking during playback, which puts a hidden mode on the most-used control. Two
dedicated buttons cost two pins and no mode, and the pins were there to spend at the
time. **They are not now**, `hardware.md`'s tally having closed at zero spare, so
read this as a cost already paid rather than slack still available. The argument
never rested on the spareness; it rested on the mode.

**Seeking is silent in v1.** Position advances and the display follows, but no audio
is produced — an audible scan needs the resampler, which would give v1 a second mode.
In v2 it becomes `r = 4` on the existing rate variable.

Tap-versus-hold is discriminated in userspace, which does **not** contradict the
kernel-decoding rule: that rule is about a poll loop quantising jog velocity, whereas
this is a one-shot timer per keypress. Two consequences — the tap fires on *release*,
imperceptible for a track change; and the **hold threshold (~300-500 ms) must sit
well clear of the 30-50 ms debounce interval**.

Open — [#12](https://github.com/tamatebox/deck-pi/issues/12): what a tap does at a
folder boundary. Stopping is the simple answer. A track reaching its end is settled:
it stops, and nothing advances on its own.

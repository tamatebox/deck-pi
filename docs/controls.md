# Controls — what each one means

`hardware.md` says which switch sits on which pin. This file says what pressing it
*does*, and `decisions.md` carries the one-line rulings.

**Every rule here is the deck's own and has to carry its own reason.** This file was
once organised around one outside player's operating instructions, with each
difference from it marked as a "departure" — that reference is gone, and
`decisions.md` records why it went. What it cost while it was here was a habit of
settling a question by looking up what some other machine does, which answers a
question this deck is not asking. A single-deck bit-perfect transport with no CD, no
network, no track database and no waveform inherits no button meanings from anything.

So the test a rule has to pass below is **not** "is this what a player does". It is
whether the rule is the only sensible reading of its own state, whether it holds for
everything on the stick, and whether it costs a mode. Where a rule is a choice
between defensible answers rather than a consequence, it says so.

**One rule carries more of this file than any other**, and it belongs to this deck:
*nothing produces sound that the operator did not press PLAY for.*

## Browsing: the encoder, ENTER and BACK

**The tree is the index**, so browsing is walking it: the encoder moves within a
folder, ENTER goes down, BACK comes up. `src/browser.rs` is the model.

| Control | What it does |
|---|---|
| Browse encoder | Moves the selection one row per detent. **Does not wrap** at either end |
| ENTER | Descends into the selected folder, or **loads** the selected file, paused at frame zero |
| BACK | Up one level, with the selection landing on the folder just left. At the root it does nothing |

**Not wrapping is a decision and `decisions.md` carries it**: a detented encoder
gives no feedback that a list has ended, and on the smallest candidate panel's three
browsable rows, arriving back at the top is indistinguishable from a mis-scroll. The
ends of a list are places you can feel.

**ENTER on a file the deck cannot play does nothing, and that is not a silent
failure.** Every rejection is decidable from the header, so the browser vets on
*highlight* and the row already carries the reason — `decisions.md`'s "say *why*,
not just *that*". By the time ENTER is reachable the answer is on the glass, so
there is nothing left for the press to say.

**ENTER acts on the selection; SEARCH and TRACK SEARCH act on the playing track.**
Those are two different things and move independently — browsing one folder while
another track plays is what a browser on a deck is *for*. `src/loaded.rs` exists
because nothing owned the second of them, and the defect that came of it was a cue
written against the highlighted row rather than the audible one.

**ENTER is the encoder's own push switch today**, and whether it earns a button of
its own is [#4](https://github.com/tamatebox/deck-pi/issues/4) — a question about how
badly cheap push switches bounce, on the most-used control. Its original framing as
a *swap* for one Pi pin is void: the controls are on a Pico and the firmware already
reads one control from two sources at once.

## PLAY / PAUSE

One button, and it toggles. Playing, it pauses where it is; paused, it plays from
where it is. It starts nothing that is not loaded — the transport refuses every
control while nothing is loaded, in one place rather than at each caller.

**It fires on the press, however long it is held**, and that is worth stating
because the obvious uniform rule would break it: under tap-versus-hold, PLAY held a
little long emits a hold and never a tap, so the deck does not start. The discipline
that argument was written against is gone; the argument is why PLAY is still the
simplest kind of button there is.

## CUE

One button, three behaviours, selected by the transport's own state and never by a
mode.

| State | Tap CUE | The name used for it |
|---|---|---|
| Paused | **sets** the cue point at the paused position | Setting Cue |
| Playing | **returns** to the cue point and pauses there | Back Cue |
| Held at the cue point | **plays while held** | Cue Point Sampler |

Those three names are the code's as well — `Cued::Set`, `Cued::Returned` and
`Cued::Previewing` in `src/transport.rs` — so a sentence here and a branch there name
one thing.

**Nothing selects between them but the deck's own state**, which is what keeps this
one button rather than a button and a modifier. Each is also the only sensible
reading of the state it fires in: paused, you are marking where you are; playing, you
want to get back to the mark; already at the mark with the button down, you want to
hear what is there without committing to it.

Four details, each falling out of a rule the deck already has rather than standing on
its own:

- **One cue point per track.** Setting a new one cancels the old. A second point
  needs a second button and the panel has none to spare — and a transport that plays
  one piece at a time has no use for a set of hot cues.
- **Setting it makes no sound.** Nothing in that branch starts the transport, so this
  holds by construction rather than by rule.
- **Back Cue pauses; it does not resume.** Returning to a point is not a press of
  PLAY, so playback restarts only when PLAY says so.
- **The preview is momentary.** Release means stop and return, with no latching. A
  latch would leave the deck making sound with nobody holding a button.

**There is no separate STOP, and this deck needs none.** Returning to the cue point
and standing by *is* stopping: the position is somewhere known, the output is silent,
and PLAY starts from there. So the button reads `CUE / STOP` and is one function, and
the hold gesture is free for the preview instead of being spent on a stop the
transport already has. It costs no new mechanism either — hold is `r = 1.0`, release
is `r = 0` with the position set back.

**CUE during a held SEARCH is Back Cue, and that is a choice rather than a
consequence.** Anything not paused counts as moving, and returning to the point is
the predictable answer. Ignoring the press would be worse: a control that sometimes
does nothing is harder to trust than one that always does the same thing.

**Auto cue is not adopted.** The mechanism is a familiar one — on load, skip the
silent lead-in and place the cue where sound starts, on a threshold somewhere in the
region of -36 to -78 dB. It is refused on the quiet case alone: one piece that opens
below the threshold on purpose is enough to make an automatic decision wrong, and a
rule has to be right for everything on the stick. The cue starts at frame zero unless
set.

Fine-adjusting the cue in single frames while paused at it would fall naturally to
SEARCH, which means nothing in that state today. Free if ever wanted.

## SEARCH, and TRACK SEARCH

**Two pairs, one meaning each.** SEARCH seeks within the track while held.
TRACK SEARCH loads the next or previous track and **waits at its head — it does
not start playing, even if the deck was playing.**

**Waiting at the head is the deck's rule and nothing else's.** An earlier version of
this file justified it by attributing it to another player, and the attribution was
false — that machine plays on through a track change. The rule survived the
correction unchanged, which is the useful part: the attribution was never what made
it right, and it has gone now along with the reference it pointed at. What makes it
right is the one rule above. A track arriving already playing is sound the operator
did not press PLAY for, and in a venue that is the worst version of it, because
attention is elsewhere.

It buys the software something as well, worth naming because `src/app/track.rs`
depends on it: **a track change always contains a pause**, which is what lets the
threads and the sink be per-track and every drop happen off the deadline. A
load-and-play control added later is what would have to revisit that module.

**They used to be one pair.** FF/REW carried both meanings: hold to seek, tap to
change track. The CDJ-200 switch panel put six buttons on one wire, so the pins that
forced the compression stopped existing and the pair split — `cdj-200.md`. Three
things changed with it, and none is cosmetic:

- **The seek starts 400 ms earlier**, on the press. There is no second meaning
  left to wait out.
- **Nothing in the input path is timed any more.** Tap-versus-hold was the only
  thing that read a clock; `src/input.rs` has two disciplines now, not three.
- **The interval rule below has nothing left to constrain.** Debounce still
  matters; the hold threshold it had to stay clear of is gone.

**Each pair earns its keep on different material.** Hold-to-seek is what makes an
80-minute piece usable at all with no jog until v2, and stepping tracks carries the
weight across a folder of short ones. The compression's stated cost was two pins, on
a header `hardware.md` had closed at zero spare. **That accounting is void**: the
controls are on a Pico, and the CDJ-200's panel reports six buttons on a single
analog line. What the argument *really* rested on is untouched and still worth
having — **it is not a mode**. The idea it rejects is overloading the browse encoder,
browsing in the list and seeking during playback, which puts a hidden mode on the
most-used control.

**Seeking is silent in v1.** Position advances and the display follows, but no audio
is produced — an audible scan needs the resampler, which would give v1 a second mode.
In v2 it becomes `r = 4` on the existing rate variable.

**Debounce is 30-50 ms and that is the only interval left.** It used to have to
sit well clear of a 300-500 ms hold threshold, and the threshold is gone with the
discipline that needed it. `implementation.md` is what says where the debounce
happens — in the Pico's firmware, not the kernel, since the controls moved.

**The panel's own six buttons cannot be read like pins**, and that does bring one
new interval: the CDJ-200's ladder passes through other buttons' voltages on the
way to its own, measured, so a level must hold for the debounce count before it
is believed. `cdj-200.md` has the numbers.

**At a folder boundary, TRACK SEARCH stops** —
[#12](https://github.com/tamatebox/deck-pi/issues/12), settled and closed.
`Loaded::neighbour` returns `None` at either end and the caller does nothing, which
is what stopping is, and it is the same rule a track reaching its end already
follows: nothing advances on its own. Folders are skipped on the way — TRACK SEARCH
means next *track*, and stepping onto a folder would be a load that fails.

**FOLDER SEARCH is the half of that question still open.** The CDJ-200 panel carries
two more buttons, the firmware decodes both of them, and the deck has no meaning for
either — so they deliberately emit nothing rather than falling through into the idle
level, where a working button would be indistinguishable from a broken wire.
`cdj-200.md` has the levels. What they should *do* is undecided and is not decided
here.

//! The display half of the loop: when to redraw, what to show, and where.
//!
//! `src/display.rs` decides what text goes in which cell and `display::paint`
//! turns that into pixels, but neither is ever *called* — the same state the
//! media watch was in for four stages. This is the caller: it watches the
//! deck, decides that something moved, builds a [`Screen`] and hands it to
//! whatever is showing one.
//!
//! # The seam, and why there is a trait here
//!
//! What shows a `Screen` is not decided. The panel is not bought
//! ([#2](https://github.com/tamatebox/deck-pi/issues/2)), the USB packer that
//! would carry pixels to the Pico is not written, and a deck with neither
//! still has to be operable — which today means text on a console. [`Show`] is
//! that seam. `display::text` implements it now; the packer implements it
//! later; a Pico that declares no panel at all falls back to the first.
//!
//! **The geometry comes from the `Show` and is asked for every turn.** Not
//! cached: the panel's geometry arrives in the Pico's declaration and a
//! reconnect can bring a different one, so a cached copy is a listing composed
//! for a panel that is no longer there.
//!
//! # Redrawing is decided from a fingerprint, not from a render
//!
//! The obvious implementation composes a `Screen` every turn and compares it
//! with the last — `Screen` is `PartialEq`, so it reads as free. It is not:
//! `Browser::view` opens the header of every file it is about to show, and
//! `architecture.md` is explicit that header reads must be driven "from what
//! is actually rendered rather than from each encoder event". Composing at
//! 100 Hz to decide not to draw is exactly what that forbids.
//!
//! So each turn reads a handful of values that are already cheap — the
//! browser's folder and selected index, the transport's state, the loaded
//! path — and only a change in one of those reaches [`Cadence`]. The render
//! happens on the far side of the coalescing window, once.

use std::path::PathBuf;

use crate::app::deck::Deck;
use crate::display::{
    compose, status_line, Cadence, Geometry, Lighting, Redraw, Screen, State, BLANK_AFTER,
    DIM_AFTER,
};
use crate::sink::AudioSink;

/// Where a [`Screen`] goes.
///
/// Three methods, and the split between the last two is
/// [`Redraw::Position`]: a position readout has to advance while nothing else
/// is changing, and sending a whole frame once a second for a clock is what
/// the cadence rules exist to avoid.
pub trait Show {
    /// The grid this display has **now**. See the module doc: asked every
    /// turn, never remembered.
    fn geometry(&self) -> Geometry;

    /// The whole screen.
    fn show(&mut self, screen: &Screen) -> std::io::Result<()>;

    /// The status line alone. Defaulted to a full redraw, because a display
    /// that cannot do less is correct and merely wasteful — and because
    /// getting this wrong silently leaves a stale position on the glass.
    fn show_status(&mut self, screen: &Screen) -> std::io::Result<()> {
        self.show(screen)
    }

    /// Dim, blank, or bring it back.
    ///
    /// Defaulted to doing nothing: a console has no brightness, and a panel
    /// that declares no levels has none either. **Not an error in that case** —
    /// a deck whose display cannot dim is a deck that does not dim, not a deck
    /// that fails.
    fn lighting(&mut self, _: Lighting) -> std::io::Result<()> {
        Ok(())
    }

    /// Something that is not the screen: a stick arriving, a refused press.
    ///
    /// Defaulted to doing nothing, because most displays have nowhere to put
    /// it — a 128x64 panel has no line to spare for a log. A console does,
    /// and while the console is the only display the deck has, this is how
    /// the deck says anything at all.
    fn note(&mut self, _line: &str) -> std::io::Result<()> {
        Ok(())
    }
}

/// The cheap read taken every turn to decide whether anything moved.
///
/// Every field is an atomic load, an integer, or a borrow — no `read_dir`, no
/// header open, no allocation unless something actually changed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    folder: Option<PathBuf>,
    selected: usize,
    state: State,
    loaded: Option<PathBuf>,
    geometry: Geometry,
}

fn fingerprint<S>(deck: &mut Deck<S>, geometry: Geometry) -> Fingerprint
where
    S: AudioSink + Send + 'static,
{
    let loaded = deck.loaded().path().map(|p| p.to_path_buf());
    let state = deck.transport().state();
    let (folder, selected) = match deck.browser() {
        Some(b) => (Some(b.path().to_path_buf()), b.selected_index()),
        None => (None, 0),
    };
    Fingerprint {
        folder,
        selected,
        state,
        loaded,
        geometry,
    }
}

/// What the folder line says when there is no medium.
///
/// `decisions.md`'s own phrase for the state, kept so the panel and the
/// documents use one word for one thing.
pub const NO_MEDIUM: &str = "No USB";

/// Builds the screen the deck would show right now.
///
/// Public and taking the deck rather than living on it, so it can be called
/// with a deck in a known state in a test without a display of any kind.
pub fn screen_of<S>(deck: &mut Deck<S>, g: Geometry) -> Screen
where
    S: AudioSink + Send + 'static,
{
    let state = deck.transport().state();
    let at = (state != State::Stopped)
        .then(|| std::time::Duration::from_secs_f64(position_secs(deck)));
    // The rate and depth are the **loaded track's**, which is what the deck
    // believes it is playing. `decisions.md` is blunt that this is not a
    // confirmation of anything — a deck wrong about its own output would
    // print the wrong number with the same confidence.
    let (rate, bits) = match deck.loaded().track() {
        Some(t) => (Some(t.rate), Some(depth_bits(t))),
        None => (None, None),
    };
    let status = status_line(state, rate, bits, at, g);

    let Some(browser) = deck.browser() else {
        return Screen {
            folder: NO_MEDIUM.to_owned(),
            lines: Vec::new(),
            status,
        };
    };
    // The current folder, not the path — `panel-compare` turned that up as a
    // finding rather than a choice.
    let folder = browser
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| browser.path().to_string_lossy().into_owned());
    let rows = browser.view(g.listing_rows());
    compose(&folder, &rows, status, g)
}

fn position_secs<S>(deck: &Deck<S>) -> f64
where
    S: AudioSink + Send + 'static,
{
    let frames = deck.transport().position();
    match deck.loaded().track() {
        Some(t) if t.rate > 0 => frames / f64::from(t.rate),
        _ => 0.0,
    }
}

fn depth_bits(track: &crate::file::TrackInfo) -> u16 {
    match track.depth {
        crate::file::Depth::Int16 => 16,
        crate::file::Depth::Int24 => 24,
    }
}

/// The display half of the app loop.
pub struct Panel<D: Show> {
    show: D,
    cadence: Cadence,
    last: Option<Fingerprint>,
    /// When the deck was last doing something. Reset by any change and by the
    /// transport running; the idle timers are measured from it.
    active_at: Option<std::time::Duration>,
    lighting: Lighting,
}

impl<D: Show> Panel<D> {
    pub fn new(show: D) -> Panel<D> {
        Panel {
            show,
            cadence: Cadence::new(),
            last: None,
            active_at: None,
            lighting: Lighting::Full,
        }
    }

    pub fn show(&self) -> &D {
        &self.show
    }

    /// The display itself, for a caller that wants to [`Show::note`] on it.
    pub fn show_mut(&mut self) -> &mut D {
        &mut self.show
    }

    /// Throws away everything remembered about what is on the glass.
    ///
    /// **For a display that has been replaced**, which is the same shape as
    /// the medium swap in [`crate::app::medium`]: a new panel is blank, but a
    /// [`Cadence`] carried over from the old one has nothing pending and a
    /// recent full draw, so it answers [`Redraw::Nothing`] — and with a track
    /// playing it will only ever answer [`Redraw::Position`]. The listing
    /// stays blank until something unrelated changes.
    pub fn forget(&mut self) {
        self.cadence = Cadence::new();
        self.last = None;
        // A replaced panel is lit however its firmware left it, which is not
        // what this one remembers. Forgetting the state is not enough — the
        // next turn has to *say* it, so the remembered value is set to
        // something the policy will disagree with.
        self.lighting = Lighting::Blank;
    }

    /// How the panel is lit right now.
    pub fn lighting(&self) -> Lighting {
        self.lighting
    }

    /// One turn. Returns what was drawn, if anything.
    pub fn turn<S>(&mut self, now: std::time::Duration, deck: &mut Deck<S>) -> std::io::Result<Redraw>
    where
        S: AudioSink + Send + 'static,
    {
        let g = self.show.geometry();
        let now_print = fingerprint(deck, g);
        let changed = self.last.as_ref() != Some(&now_print);
        // "Moving" is the position advancing, which is what the slow tick is
        // for. Seeking counts: the position moves and the readout has to
        // follow it, silent or not.
        let moving = matches!(
            now_print.state,
            State::Playing | State::SeekingForward | State::SeekingBack
        );

        // **Before the draw, so a frame never lands on a blanked panel.**
        // Waking and drawing in the other order would put the new listing on
        // glass that is still off, and the unblank a moment later would show
        // it — which works, and would stop working the moment a panel needs
        // its charge pump settled before it takes pixels.
        self.light(now, changed, moving)?;

        match self.cadence.poll(now, changed, moving) {
            Redraw::Nothing => Ok(Redraw::Nothing),
            Redraw::Position => {
                let screen = screen_of(deck, g);
                self.show.show_status(&screen)?;
                Ok(Redraw::Position)
            }
            Redraw::Full => {
                let screen = screen_of(deck, g);
                self.show.show(&screen)?;
                self.last = Some(now_print);
                Ok(Redraw::Full)
            }
        }
    }

    /// The idle policy, which is one sentence of `architecture.md` and one
    /// trap.
    ///
    /// **The trap is the gate.** Dimming on "no input for a while" is the
    /// obvious reading and it is wrong here: with a long track playing, no
    /// input for a few minutes is *normal*, and blanking then hides the
    /// position readout exactly when someone is watching it — in a dark room,
    /// mid-set. So the clock only runs when **nothing is playing**, which is
    /// what the document means by "idle means nothing playing".
    ///
    /// Seeking counts as playing. The position is moving and is being read,
    /// which is the whole reason FF and REW are silent rather than absent.
    fn light(&mut self, now: std::time::Duration, changed: bool, moving: bool) -> std::io::Result<()> {
        if changed || moving || self.active_at.is_none() {
            self.active_at = Some(now);
        }
        let idle = now.saturating_sub(self.active_at.unwrap_or(now));
        let want = if moving || idle < DIM_AFTER {
            Lighting::Full
        } else if idle < BLANK_AFTER {
            Lighting::Dim
        } else {
            Lighting::Blank
        };
        if want != self.lighting {
            self.show.lighting(want)?;
            self.lighting = want;
        }
        Ok(())
    }
}

//! The input decoder against the transport, which is the seam it exists for.
//!
//! `src/input.rs`'s own tests cover the gesture rules. What needs putting
//! together is that the gestures line up with the calls the transport
//! actually exposes — `cue_down`/`cue_up` as a pair, `begin_seek`/`end_seek`
//! as a pair, and PLAY as a single edge.
//!
//! **The browser-facing half is deliberately not here.** `Tap(Ff)` means "next
//! track", and the documents never say *relative to what* — the browser holds
//! a selection, the deck holds a playing track, and you can browse one folder
//! while another plays. That is an undecided question, not something to settle
//! inside a test.

use std::time::Duration;

use deck_pi::input::{Action, Button, Decoder, RawEvent, EV_KEY};
use deck_pi::transport::{State, Transport, RATE_PAUSED, RATE_SEEK, RATE_UNITY};

fn key(button: Button, value: i32) -> RawEvent {
    RawEvent {
        kind: EV_KEY,
        code: button.keycode(),
        value,
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The dispatch the application will do, written out here because the seam is
/// what is under test.
///
/// It carries one piece of state of its own, and finding that is the point:
/// **`end_seek` needs to know whether the deck was playing before the seek
/// began**, and neither the input layer nor the transport remembers it. The
/// gesture pair is where it belongs, so whatever owns the app loop has to
/// hold it.
struct Deck {
    transport: Transport,
    was_playing: bool,
}

impl Deck {
    fn new() -> Deck {
        Deck {
            transport: Transport::new(),
            was_playing: false,
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Press(Button::PlayPause) => {
                if self.transport.rate() == RATE_PAUSED {
                    self.transport.play();
                } else {
                    self.transport.pause();
                }
            }
            Action::Press(Button::Cue) => self.transport.cue_down(),
            Action::Release(Button::Cue) => self.transport.cue_up(),
            Action::HoldStart(b @ (Button::Ff | Button::Rew)) => {
                self.was_playing = self.transport.rate() != RATE_PAUSED;
                self.transport.begin_seek(b == Button::Ff);
            }
            Action::HoldEnd(Button::Ff | Button::Rew) => {
                self.transport.end_seek(self.was_playing)
            }
            // Everything else is the browser's, and what it means is open.
            _ => {}
        }
    }
}

/// Drives the decoder and applies whatever comes out.
fn feed(deck: &mut Deck, d: &mut Decoder, now: Duration, ev: RawEvent) {
    let mut out = Vec::new();
    d.feed(now, ev, &mut out);
    for a in out {
        deck.apply(a);
    }
}

fn tick(deck: &mut Deck, d: &mut Decoder, now: Duration) {
    let mut out = Vec::new();
    d.tick(now, &mut out);
    for a in out {
        deck.apply(a);
    }
}

#[test]
fn play_toggles_on_each_press_and_a_long_press_is_still_one_toggle() {
    // The failure a uniform tap-or-hold rule would have caused, end to end:
    // PLAY held slightly long would emit a hold and never a press, so the
    // deck would not start.
    let mut deck = Deck::new();
    let mut d = Decoder::default();

    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 1));
    assert_eq!(deck.transport.rate(), RATE_UNITY, "PLAY starts on the press");

    // Held for two seconds. Nothing further may happen.
    tick(&mut deck, &mut d, ms(2_000));
    assert_eq!(deck.transport.rate(), RATE_UNITY);
    feed(&mut deck, &mut d, ms(2_000), key(Button::PlayPause, 0));
    assert_eq!(deck.transport.rate(), RATE_UNITY, "releasing changes nothing");

    feed(&mut deck, &mut d, ms(3_000), key(Button::PlayPause, 1));
    assert_eq!(deck.transport.rate(), RATE_PAUSED, "and the next press pauses");
}

#[test]
fn holding_ff_seeks_and_releasing_restores_what_was_playing() {
    // The pair the `Momentary` / `TapOrHold` split exists to produce, and the
    // piece of state that has to live between them.
    let mut deck = Deck::new();
    let mut d = Decoder::default();

    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 1));
    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 0));
    assert_eq!(deck.transport.rate(), RATE_UNITY);

    feed(&mut deck, &mut d, ms(100), key(Button::Ff, 1));
    tick(&mut deck, &mut d, ms(300));
    assert_eq!(deck.transport.rate(), RATE_UNITY, "not held long enough yet");

    tick(&mut deck, &mut d, ms(500));
    assert_eq!(deck.transport.rate(), RATE_SEEK, "the hold starts the seek");
    assert_eq!(deck.transport.state(), State::SeekingForward);
    assert!(
        deck.transport.is_silent(),
        "v1's seek is silent — an audible scan would need the resampler"
    );

    feed(&mut deck, &mut d, ms(2_000), key(Button::Ff, 0));
    assert_eq!(deck.transport.rate(), RATE_UNITY, "playing resumes");
    assert!(!deck.transport.is_silent());
}

#[test]
fn seeking_from_a_pause_returns_to_a_pause() {
    // The other half of the state `end_seek` needs. Getting this wrong would
    // start the deck playing when a seek ended, which in a venue is the
    // wrong direction of error.
    let mut deck = Deck::new();
    let mut d = Decoder::default();
    assert_eq!(deck.transport.rate(), RATE_PAUSED);

    feed(&mut deck, &mut d, ms(0), key(Button::Rew, 1));
    tick(&mut deck, &mut d, ms(400));
    assert_eq!(deck.transport.state(), State::SeekingBack);

    feed(&mut deck, &mut d, ms(1_000), key(Button::Rew, 0));
    assert_eq!(deck.transport.rate(), RATE_PAUSED, "it must not start playing");
}

#[test]
fn a_short_ff_never_touches_the_transport() {
    // A tap is a track change, which is the browser's business — the
    // transport must see nothing at all, or a tap would nudge the position.
    let mut deck = Deck::new();
    let mut d = Decoder::default();
    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 1));
    let before = deck.transport.state();

    feed(&mut deck, &mut d, ms(1_000), key(Button::Ff, 1));
    feed(&mut deck, &mut d, ms(1_050), key(Button::Ff, 0));
    assert_eq!(deck.transport.state(), before);
    assert_eq!(deck.transport.rate(), RATE_UNITY);
}

#[test]
fn cue_held_at_the_point_previews_and_releasing_it_returns() {
    // Cue Point Sampler: "playback continues while the button is held in",
    // so it must start on the press. A threshold would have made the preview
    // arrive 400 ms late, which is the whole reason CUE is `Momentary`.
    let mut deck = Deck::new();
    let mut d = Decoder::default();

    // Paused at the cue point, which is frame zero until set.
    assert_eq!(deck.transport.rate(), RATE_PAUSED);
    assert_eq!(deck.transport.cue_point(), 0);

    feed(&mut deck, &mut d, ms(0), key(Button::Cue, 1));
    assert_eq!(deck.transport.rate(), RATE_UNITY, "the preview starts at once");

    feed(&mut deck, &mut d, ms(120), key(Button::Cue, 0));
    assert_eq!(deck.transport.rate(), RATE_PAUSED, "release stops it");
    assert_eq!(deck.transport.take_seek(), Some(0), "and returns to the point");
}

#[test]
fn cue_while_paused_away_from_the_point_sets_it_and_makes_no_sound() {
    // Setting Cue. "No sound is output at this time."
    let mut deck = Deck::new();
    let mut d = Decoder::default();
    deck.transport.pause();
    deck.transport.publish_position(150_000.0);

    feed(&mut deck, &mut d, ms(0), key(Button::Cue, 1));
    feed(&mut deck, &mut d, ms(50), key(Button::Cue, 0));

    assert_eq!(deck.transport.cue_point(), 150_000);
    assert_eq!(deck.transport.rate(), RATE_PAUSED, "setting a cue is silent");
}

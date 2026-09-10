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

use deck_pi::app::deck::Deck;
use deck_pi::app::track::Config;
use deck_pi::input::{Button, Decoder, RawEvent, EV_KEY};
use deck_pi::sink::CaptureSink;
use deck_pi::transport::{State, RATE_PAUSED, RATE_SEEK, RATE_UNITY};

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

/// The **real** dispatch, driven by the real decoder.
///
/// This was a hand-written copy of `Deck::apply` living in the test file, and
/// the copy is exactly what these tests could not check: a divergence between
/// the two would have left every assertion here green. `implementation.md`'s
/// rule is that agreement between a description and its code is evidence
/// about the description — a second implementation is a description.
///
/// The one thing the copy did earn: **`end_seek` needs to know whether the
/// deck was playing before the seek began**, and neither the input layer nor
/// the transport remembers it. Writing the dispatch out here is what found
/// that, and it is now `Deck::was_playing`.
///
/// **No track is loaded and the transport is put into the loaded state by
/// hand.** A deck with nothing loaded refuses every control — the state
/// machine's own rule, tested where it belongs — and these eight tests are
/// about what a *gesture* means, not about the lifecycle, so starting two
/// threads and a sink for each would buy nothing and cost seconds.
fn deck() -> Deck<CaptureSink> {
    let d = Deck::new(
        Box::new(|_| unreachable!("these tests load no track")),
        None,
        None,
        Config::default(),
    );
    d.transport().track_loaded(0);
    d
}

/// Drives the decoder and applies whatever comes out.
fn feed(deck: &mut Deck<CaptureSink>, d: &mut Decoder, now: Duration, ev: RawEvent) {
    let mut out = Vec::new();
    d.feed(now, ev, &mut out);
    for a in out {
        deck.apply(a).expect("the dispatch must not fail with no medium");
    }
}

fn tick(deck: &mut Deck<CaptureSink>, d: &mut Decoder, now: Duration) {
    let mut out = Vec::new();
    d.tick(now, &mut out);
    for a in out {
        deck.apply(a).expect("the dispatch must not fail with no medium");
    }
}

#[test]
fn play_toggles_on_each_press_and_a_long_press_is_still_one_toggle() {
    // The failure a uniform tap-or-hold rule would have caused, end to end:
    // PLAY held slightly long would emit a hold and never a press, so the
    // deck would not start.
    let mut deck = deck();
    let mut d = Decoder::default();

    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 1));
    assert_eq!(deck.transport().rate(), RATE_UNITY, "PLAY starts on the press");

    // Held for two seconds. Nothing further may happen.
    tick(&mut deck, &mut d, ms(2_000));
    assert_eq!(deck.transport().rate(), RATE_UNITY);
    feed(&mut deck, &mut d, ms(2_000), key(Button::PlayPause, 0));
    assert_eq!(deck.transport().rate(), RATE_UNITY, "releasing changes nothing");

    feed(&mut deck, &mut d, ms(3_000), key(Button::PlayPause, 1));
    assert_eq!(deck.transport().rate(), RATE_PAUSED, "and the next press pauses");
}

#[test]
fn holding_ff_seeks_and_releasing_restores_what_was_playing() {
    // The pair the `Momentary` / `TapOrHold` split exists to produce, and the
    // piece of state that has to live between them.
    let mut deck = deck();
    let mut d = Decoder::default();

    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 1));
    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 0));
    assert_eq!(deck.transport().rate(), RATE_UNITY);

    feed(&mut deck, &mut d, ms(100), key(Button::Ff, 1));
    tick(&mut deck, &mut d, ms(300));
    assert_eq!(deck.transport().rate(), RATE_UNITY, "not held long enough yet");

    tick(&mut deck, &mut d, ms(500));
    assert_eq!(deck.transport().rate(), RATE_SEEK, "the hold starts the seek");
    assert_eq!(deck.transport().state(), State::SeekingForward);
    assert!(
        deck.transport().is_silent(),
        "v1's seek is silent — an audible scan would need the resampler"
    );

    feed(&mut deck, &mut d, ms(2_000), key(Button::Ff, 0));
    assert_eq!(deck.transport().rate(), RATE_UNITY, "playing resumes");
    assert!(!deck.transport().is_silent());
}

#[test]
fn seeking_from_a_pause_returns_to_a_pause() {
    // The other half of the state `end_seek` needs. Getting this wrong would
    // start the deck playing when a seek ended, which in a venue is the
    // wrong direction of error.
    let mut deck = deck();
    let mut d = Decoder::default();
    assert_eq!(deck.transport().rate(), RATE_PAUSED);

    feed(&mut deck, &mut d, ms(0), key(Button::Rew, 1));
    tick(&mut deck, &mut d, ms(400));
    assert_eq!(deck.transport().state(), State::SeekingBack);

    feed(&mut deck, &mut d, ms(1_000), key(Button::Rew, 0));
    assert_eq!(deck.transport().rate(), RATE_PAUSED, "it must not start playing");
}

#[test]
fn a_short_ff_never_touches_the_transport() {
    // A tap is a track change, which is the browser's business — the
    // transport must see nothing at all, or a tap would nudge the position.
    let mut deck = deck();
    let mut d = Decoder::default();
    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 1));
    let before = deck.transport().state();

    feed(&mut deck, &mut d, ms(1_000), key(Button::Ff, 1));
    feed(&mut deck, &mut d, ms(1_050), key(Button::Ff, 0));
    assert_eq!(deck.transport().state(), before);
    assert_eq!(deck.transport().rate(), RATE_UNITY);
}

#[test]
fn cue_held_at_the_point_previews_and_releasing_it_returns() {
    // Cue Point Sampler: "playback continues while the button is held in",
    // so it must start on the press. A threshold would have made the preview
    // arrive 400 ms late, which is the whole reason CUE is `Momentary`.
    let mut deck = deck();
    let mut d = Decoder::default();

    // Paused at the cue point, which is frame zero until set.
    assert_eq!(deck.transport().rate(), RATE_PAUSED);
    assert_eq!(deck.transport().cue_point(), 0);

    feed(&mut deck, &mut d, ms(0), key(Button::Cue, 1));
    assert_eq!(deck.transport().rate(), RATE_UNITY, "the preview starts at once");

    feed(&mut deck, &mut d, ms(120), key(Button::Cue, 0));
    assert_eq!(deck.transport().rate(), RATE_PAUSED, "release stops it");
    assert_eq!(deck.transport().peek_seek(), Some(0), "and returns to the point");
}

#[test]
fn cue_while_paused_away_from_the_point_sets_it_and_makes_no_sound() {
    // Setting Cue. "No sound is output at this time."
    let mut deck = deck();
    let mut d = Decoder::default();
    deck.transport().pause();
    deck.transport().publish_position(150_000.0);

    feed(&mut deck, &mut d, ms(0), key(Button::Cue, 1));
    feed(&mut deck, &mut d, ms(50), key(Button::Cue, 0));

    assert_eq!(deck.transport().cue_point(), 150_000);
    assert_eq!(deck.transport().rate(), RATE_PAUSED, "setting a cue is silent");
}

#[test]
fn cue_during_a_held_seek_pauses_and_releasing_the_button_does_not_undo_it() {
    // Back Cue while FF is held. `hardware.md` chooses this interpretation
    // deliberately — "anything not paused counts as moving, and returning to
    // the point is the predictable answer" — and the CDJ-350 rule it inherits
    // is quoted in `decisions.md`: **"Back Cue pauses; it does not resume."**
    //
    // The release used to undo it. `was_playing` below is captured when FF
    // goes down, and nothing updates it when CUE changes the state, so
    // `end_seek(true)` started playback that Back Cue had just stopped. The
    // deck would resume, from the cue point, with no button pressed to say so.
    //
    // Fixed in the transport rather than here: `end_seek` now returns unless
    // the deck is still seeking. Putting it in this struct would have left
    // every other caller — including the app loop, which does not exist
    // yet — free to make the same mistake.
    let mut deck = deck();
    let mut d = Decoder::default();

    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 1));
    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 0));
    deck.transport().publish_position(500_000.0);
    assert_eq!(deck.transport().rate(), RATE_UNITY);

    feed(&mut deck, &mut d, ms(100), key(Button::Ff, 1));
    tick(&mut deck, &mut d, ms(500));
    assert_eq!(deck.transport().state(), State::SeekingForward);

    // CUE, still holding FF.
    feed(&mut deck, &mut d, ms(700), key(Button::Cue, 1));
    feed(&mut deck, &mut d, ms(760), key(Button::Cue, 0));
    assert_eq!(deck.transport().state(), State::Paused, "Back Cue pauses");
    assert_eq!(deck.transport().peek_seek(), Some(0), "and returns to the point");

    // Now let FF go. This must change nothing.
    feed(&mut deck, &mut d, ms(1_200), key(Button::Ff, 0));
    assert_eq!(
        deck.transport().state(),
        State::Paused,
        "releasing FF must not resume playback Back Cue stopped"
    );
    assert_eq!(deck.transport().rate(), RATE_PAUSED);
}

#[test]
fn play_pressed_during_a_held_seek_is_not_swallowed_by_the_release() {
    // The same staleness from the other side. PLAY during a held FF pauses
    // (the deck is not at rate zero, so the toggle pauses), and the release
    // used to restore `was_playing` and undo it — so the press did nothing at
    // all. A control that sometimes does nothing is the thing `hardware.md`
    // says is harder to trust than one that always does the same.
    let mut deck = deck();
    let mut d = Decoder::default();

    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 1));
    feed(&mut deck, &mut d, ms(0), key(Button::PlayPause, 0));
    feed(&mut deck, &mut d, ms(100), key(Button::Ff, 1));
    tick(&mut deck, &mut d, ms(500));
    assert_eq!(deck.transport().state(), State::SeekingForward);

    feed(&mut deck, &mut d, ms(700), key(Button::PlayPause, 1));
    assert_eq!(deck.transport().rate(), RATE_PAUSED, "PLAY takes effect at once");

    feed(&mut deck, &mut d, ms(1_200), key(Button::Ff, 0));
    assert_eq!(
        deck.transport().rate(),
        RATE_PAUSED,
        "and the release does not put back what PLAY changed"
    );
}

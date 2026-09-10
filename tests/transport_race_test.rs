//! The `rate`/`silent` pair, under a control thread that keeps changing it.
//!
//! `Transport`'s fields are each a single atomic, which the type's own doc
//! used to offer as the reason there was nothing to guard. That is true of
//! each field and was false of the **pair** `Engine::step` decides on: it
//! reads `rate`, then `is_silent()`, and two adjacent loads against two
//! adjacent stores are enough to see a combination that never existed. No
//! memory reordering required, and so nothing here is `#[ignore]`d or timing
//! sensitive in the way `ring_race_test.rs` is — the defect it guards was
//! measured at roughly a third of all fills.
//!
//! The combination that mattered was a seek rate with `silent == false`, which
//! `step` reports as `NeedsResampler`. v1 has no resampler, so `main.rs`
//! treats that as fatal: a rare interleaving became a stopped deck mid-set.
//!
//! **Sensitivity, measured, because `ring_race_test.rs` taught us not to trust
//! a green probabilistic test.** Reverting `Engine::fill` to two separate
//! loads and changing nothing else: **3 detections in 3 runs**, at 1,156,
//! 1,453 and 1,472 hits per run out of ~30M fills. Unlike the ring's, this
//! detector is reliable, which is why it runs in the default suite.
//!
//! Two intermediate fixes are recorded here because both look sufficient and
//! neither is. **Reordering the stores** so `silent` is never cleared while
//! the rate is still a seek rate took it from ~10.7M in 35.6M to 49 in 27.7M
//! — three orders of magnitude, and not zero. **Adding `Release` to the clear
//! and `Acquire` to the read** took it to 35 in 33.8M. It cannot reach zero
//! that way at all: the reader's two loads can straddle the *entire* write,
//! reading the old rate and then the new flag, and no ordering of the writer's
//! stores can prevent a reader from having already read the first value.
//! Only reading the pair as one thing fixes it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use deck_pi::engine::{Engine, Outcome};
use deck_pi::file::RING_CHANNELS;
use deck_pi::ring;
use deck_pi::transport::Transport;

#[test]
fn no_interleaving_of_control_writes_can_ask_v1_for_a_resampler() {
    // Long enough that the position never reaches the end during the run;
    // this test is about the transport pair, not about track boundaries.
    const FRAMES: u64 = u64::MAX / 4;
    const BLOCK: usize = 128;

    let (mut w, r) = ring::new(4096);
    let block: Vec<i32> = (0..2048u64).flat_map(|f| [f as i32, f as i32]).collect();
    w.append(&block);

    let transport = Arc::new(Transport::new());
    let stop = Arc::new(AtomicBool::new(false));

    let ctl = Arc::clone(&transport);
    let ctl_stop = Arc::clone(&stop);
    let control = std::thread::spawn(move || {
        // Exactly what a held FF looks like from the control thread, as fast
        // as it can be produced. The real gap between these is a button
        // press; the point is to make the window between the two stores land
        // under the reader as often as possible.
        while !ctl_stop.load(Ordering::Relaxed) {
            ctl.play();
            ctl.begin_seek(true);
            ctl.end_seek(true);
            ctl.begin_seek(false);
            ctl.end_seek(true);
            ctl.pause();
        }
    });

    let mut engine = Engine::new(FRAMES);
    let mut out = vec![0i32; BLOCK * RING_CHANNELS];
    let mut fills = 0u64;
    let mut asked_for_a_resampler = 0u64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        for _ in 0..1_000 {
            // A miss is expected and irrelevant — the playhead runs off the
            // resident span almost immediately and stays off it. Only the
            // outcome that says "v1 needs a resampler" is under test.
            if let Outcome::NeedsResampler { .. } = engine.fill(&transport, &r, &mut out) {
                asked_for_a_resampler += 1;
            }
            fills += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    control.join().expect("control thread");

    assert!(fills > 100_000, "too few fills to mean anything: {fills}");
    assert_eq!(
        asked_for_a_resampler, 0,
        "{asked_for_a_resampler} of {fills} fills saw a seek rate with the silent \
         flag already cleared — v1 has no resampler and the caller treats this as fatal"
    );
}

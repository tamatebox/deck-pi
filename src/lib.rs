//! deck-pi — bit-perfect single-deck DJ transport.
//!
//! One process, one binary: engine, browser and display. The only boundary
//! that constrains structure is the deadline, and exactly one thing has one —
//! the audio callback (docs/architecture.md).
//!
//! Built so far: the file layer and the FFI beneath it, the ring, the window
//! thread, the transport, the callback, the output sink, the realtime process
//! setup that makes the callback's "cannot fault" true, the browser, media watch,
//! the cue store and input. Not started: the display, and the app loop that would
//! join these together.

pub mod app;
pub mod browser;
pub mod cue;
pub mod engine;
pub mod file;
pub mod input;
pub mod loaded;
pub mod media;
pub mod ring;
pub mod rt;
pub mod sink;
pub mod sndfile;
pub mod transport;
pub mod window;

/// The allocator wrapper that makes the callback discipline a check rather
/// than an intention (CLAUDE.md: enforce it from the first commit, while the
/// load is still light enough to get away with breaking it).
///
/// The crate's default feature, `disable_release`, is switched **off**, so
/// the behaviour is deliberate in both profiles:
///
/// - **debug and tests** — abort mode. An allocation inside
///   [`assert_no_alloc`] aborts immediately. This is where a violation should
///   be caught, and it is loud.
/// - **release** — `warn_release`. Violations are counted, not aborted:
///   killing the audio thread mid-set is worse than a wrong sample, and
///   `assert_no_alloc::violation_count()` is something the display can
///   surface. An allocation here is still a bug that should never have
///   shipped; this only decides how it fails if one does.
///
/// The cost is a thread-local check on every allocation process-wide. That is
/// affordable precisely because of how the program is divided: the callback
/// allocates nothing, and everything that does allocate is off the deadline.
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// `assert_no_alloc`, meaning the same thing in both profiles — **and living
/// here rather than in a test file, which is the load-bearing part.**
///
/// # A no-allocation test can be completely inert, and look identical
///
/// The allocator above is declared in *this crate*. An integration test is a
/// separate crate that depends on it, and **a test binary that never
/// references anything from the library does not get the allocator at all**:
/// the guard is simply absent and every `assert_no_alloc` in the file passes.
///
/// Measured both ways with the same body, `Vec::with_capacity(4096)` inside
/// the guard:
///
/// | Test file | Result |
/// |---|---|
/// | imports only `std` and `assert_no_alloc` | **passes**, no abort |
/// | plus one `deck_pi::ring::new(64)` | SIGABRT, "memory allocation of 32768 bytes failed" |
///
/// `tests/callback_rules.rs` is safe only by accident — it happens to use
/// `ring` everywhere. A new file asserting "the app loop allocates nothing"
/// would be the natural place to get this wrong, and it would report success.
///
/// **Calling this function is what fixes it**, because using it links the
/// crate and the crate carries the allocator. That is why it is `pub` and why
/// it is here: a convention that every such test must touch the library is a
/// convention, and this is not.
///
/// # And it checks the release profile, where the abort does not happen
///
/// `Cargo.toml` selects `warn_release`, under which a violation prints a line
/// and increments a counter — `assert_no_alloc` itself does not panic. A
/// region that only calls `assert_no_alloc` therefore passes in release
/// whether or not it allocated. Release is the profile that runs on the deck.
pub fn no_alloc<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(not(debug_assertions))]
    assert_no_alloc::reset_violation_count();
    let out = assert_no_alloc::assert_no_alloc(f);
    #[cfg(not(debug_assertions))]
    assert_eq!(
        assert_no_alloc::violation_count(),
        0,
        "allocated in release — see the line printed above"
    );
    out
}

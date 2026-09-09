//! deck-pi — bit-perfect single-deck DJ transport.
//!
//! One process, one binary: engine, browser and display. The only boundary
//! that constrains structure is the deadline, and exactly one thing has one —
//! the audio callback (docs/architecture.md).
//!
//! Built so far: the file layer and the FFI beneath it. Both sit on the side
//! of the line that is allowed to block.

pub mod engine;
pub mod file;
pub mod ring;
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

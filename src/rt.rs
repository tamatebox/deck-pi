//! Realtime process setup: locked memory, `SCHED_FIFO`, core pinning.
//!
//! `CLAUDE.md` states the callback invariant as "allocates nothing, locks
//! nothing, does no I/O, and **cannot fault**". The first three are properties
//! of the code and are checked by `tests/callback_rules.rs`. The fourth is not
//! a property of the code at all — it is a property of the *process*, and
//! without what this module does it is simply false: a page touched for the
//! first time inside the callback faults, however carefully the callback was
//! written.
//!
//! # Everything here fails silently, which is why it verifies
//!
//! This is the project's usual failure shape, three times over:
//!
//! - `mlockall` without `RLIMIT_MEMLOCK` returns `ENOMEM`. The process keeps
//!   running, unlocked.
//! - `sched_setscheduler` without `RLIMIT_RTPRIO` returns `EPERM`. The thread
//!   keeps running, at normal priority.
//! - `sched_setaffinity` to a core that does not exist returns `EINVAL`. The
//!   thread keeps running, unpinned.
//!
//! In all three cases **audio still comes out**, and on a desk it comes out
//! fine — the load is trivial and the machine is idle. What changes is the
//! behaviour under a set, twenty minutes in, on a box that is also servicing
//! GPIO interrupts. So these are not "best effort" calls: each one is applied
//! and then **read back**, the same discipline `sink::alsa::verify_in_force`
//! applies to `hw_params`, and a mismatch names the field.
//!
//! Two of the three can be answered *before* trying, from `getrlimit`, which
//! is better than an errno: it says which line of `/etc/security/limits.conf`
//! is missing rather than that something was refused.
//!
//! # What needs pre-faulting, and what `implementation.md` overstates
//!
//! That file asks for "the stack and heap". The stack, yes — the audio thread
//! touches stack pages it has never touched before on its first few periods,
//! and `MCL_FUTURE` locks a page when it is faulted, not before.
//!
//! The heap half is moot here, and worth saying so rather than writing code
//! that pretends otherwise. The callback allocates nothing, so it touches no
//! new heap page; the one large heap object in the audio path is the ring, and
//! the *window thread* writes every byte of it while filling, which faults it
//! off the deadline. Pre-faulting the heap would mean growing glibc's arena
//! and hoping `free` does not hand it back — which is where the withdrawn
//! `mallopt` note in `implementation.md` came from. Not needed, so not done.

/// The `SCHED_FIFO` range `architecture.md` specifies.
///
/// Stated as a range rather than a value because the number is a choice
/// within it, and because a value outside it should be refused loudly rather
/// than quietly accepted — 99 would outrank the kernel's own threads.
pub const PRIORITY_RANGE: std::ops::RangeInclusive<i32> = 70..=80;

/// What to ask the kernel for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtRequest {
    /// `SCHED_FIFO` priority, which must be inside [`PRIORITY_RANGE`].
    pub priority: i32,
    /// Stack bytes to touch before the deadline starts.
    pub stack_prefault_bytes: usize,
    /// Core to pin the audio thread to. `architecture.md` asks for the audio
    /// thread and GPIO interrupt handling to be on different cores; the 3B+
    /// has four and one deck to run.
    pub cpu: Option<usize>,
}

impl Default for RtRequest {
    fn default() -> Self {
        RtRequest {
            // The middle of the range, so neither bound is being tested by
            // accident.
            priority: 75,
            // Two orders of magnitude more stack than the callback's frame
            // needs. It costs one pass over 256 KiB, once.
            stack_prefault_bytes: 256 * 1024,
            cpu: None,
        }
    }
}

/// The scheduling policy actually in force, as `sched_getscheduler` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Fifo,
    RoundRobin,
    /// `SCHED_OTHER` — the ordinary timeshare policy. This is what a refused
    /// promotion leaves the thread on.
    Other,
    Unknown(i32),
}

impl std::fmt::Display for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Policy::Fifo => f.write_str("SCHED_FIFO"),
            Policy::RoundRobin => f.write_str("SCHED_RR"),
            Policy::Other => f.write_str("SCHED_OTHER"),
            Policy::Unknown(n) => write!(f, "policy {}", n),
        }
    }
}

/// The two limits that decide whether any of this can work.
///
/// Read rather than assumed, so a failure can name the missing
/// `limits.conf` line instead of an errno.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// `RLIMIT_MEMLOCK` soft limit in bytes. `None` for unlimited.
    pub memlock_bytes: Option<u64>,
    /// `RLIMIT_RTPRIO` soft limit — the highest `SCHED_FIFO` priority this
    /// user may ask for. Zero means realtime scheduling is not available.
    pub rtprio: u64,
}

impl Limits {
    /// Whether a `mlockall` of a process this size can succeed.
    pub fn allows_locking(&self, needed_bytes: u64) -> bool {
        match self.memlock_bytes {
            None => true,
            Some(limit) => limit >= needed_bytes,
        }
    }

    /// Whether `promote_current_thread` can get this priority.
    pub fn allows_priority(&self, priority: i32) -> bool {
        priority >= 0 && self.rtprio >= priority as u64
    }
}

/// What the kernel says is in force, after the fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtInForce {
    pub policy: Policy,
    pub priority: i32,
    /// `VmLck` from `/proc/self/status`, in KiB. This is the one that catches
    /// a `mlockall` that returned success and locked less than expected.
    pub locked_kib: u64,
    /// The cores this thread may run on.
    pub cpus: Vec<usize>,
}

/// Every way this can go wrong, with what to do about it where there is
/// something to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtError {
    /// A limit rules the request out before it is made. `remedy` is the
    /// `/etc/security/limits.conf` line that fixes it.
    Forbidden {
        what: &'static str,
        limit: String,
        needed: String,
        remedy: String,
    },
    /// A syscall refused.
    Refused { call: &'static str, errno: i32 },
    /// It returned success and then read back as something else — the case
    /// this module exists for.
    NotInForce {
        field: &'static str,
        asked: String,
        got: String,
    },
    /// A priority outside [`PRIORITY_RANGE`].
    PriorityOutOfRange { asked: i32 },
    /// Could not read one of the `/proc` files the verification depends on.
    Proc(String),
    /// Not Linux. The deck is Linux; this is what the development Mac gets,
    /// and it is an error rather than a silent no-op so that nothing can
    /// report a realtime setup it did not perform.
    NotSupported,
}

impl std::fmt::Display for RtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RtError::Forbidden {
                what,
                limit,
                needed,
                remedy,
            } => write!(
                f,
                "{} needs {} but the limit is {} — add to /etc/security/limits.conf:\n    {}",
                what, needed, limit, remedy
            ),
            RtError::Refused { call, errno } => {
                write!(f, "{} refused: errno {}", call, errno)
            }
            RtError::NotInForce { field, asked, got } => write!(
                f,
                "{} asked for {} and the kernel reports {}",
                field, asked, got
            ),
            RtError::PriorityOutOfRange { asked } => write!(
                f,
                "SCHED_FIFO priority {} is outside the {}-{} range architecture.md specifies",
                asked,
                PRIORITY_RANGE.start(),
                PRIORITY_RANGE.end()
            ),
            RtError::Proc(m) => write!(f, "{}", m),
            RtError::NotSupported => {
                f.write_str("realtime setup needs Linux; this is not the deck")
            }
        }
    }
}

impl std::error::Error for RtError {}

/// Parses `VmLck` out of `/proc/self/status`, in KiB.
///
/// Split from the read so the parse is testable off Linux, the same way
/// `sink::alsa::parse_proc_hw_params` is.
pub fn parse_vmlck_kib(status: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmLck:") {
            // `VmLck:        1024 kB`
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// The `limits.conf` lines this design needs, for the message on failure.
///
/// `unlimited` for memlock rather than a figure: the ring is sized per track
/// from the source rate, so the number is not fixed at configuration time,
/// and `MCL_FUTURE` brings every later allocation under the same limit.
pub fn limits_conf_lines() -> &'static str {
    "@audio - memlock unlimited\n    @audio - rtprio 80"
}

/// Page size assumed for striding. 4 KiB on the Pi and on both development
/// architectures; a larger real page only makes the striding redundant, never
/// wrong.
const PAGE: usize = 4096;

/// The most stack this will touch, whatever it is asked for.
///
/// A spawned thread's default stack is 2 MiB, and running off the end of it
/// while *preparing* for realtime would be an absurd way to fail. 1 MiB is
/// four times the default request and still a quarter of the smallest stack
/// in play.
pub const STACK_PREFAULT_CAP: usize = 1024 * 1024;

/// Touches stack pages so `MCL_FUTURE` locks them now rather than inside the
/// callback, and **returns how many bytes of stack it actually reached** so
/// the caller is not taking it on trust.
///
/// It recurses, which is the point: a loop over one local array touches the
/// same page every time and prefaults nothing at all. That was the first
/// version of this function, and it is the sort of mistake that leaves the
/// invariant reading as satisfied — the call is there, the call returns, and
/// the pages are still untouched.
///
/// `write_volatile` and `black_box` are what stop the writes being removed;
/// they are dead by every ordinary analysis, which is exactly the shape a
/// compiler deletes. Portable, because it is only stack writes — locking them
/// is the part that needs Linux.
pub fn prefault_stack(bytes: usize) -> usize {
    let pages = bytes.min(STACK_PREFAULT_CAP) / PAGE;
    if pages == 0 {
        // A request smaller than a page touches nothing, rather than one
        // frame's worth. Otherwise the returned reach would be non-zero for a
        // request of zero, which is a confusing thing for a caller to log.
        return 0;
    }
    let anchor = 0u8;
    let top = std::hint::black_box(&anchor) as *const u8 as usize;
    let deepest = touch(pages);
    // Stacks grow down on aarch64 and x86_64 alike. `saturating_sub` rather
    // than a subtraction because a platform where they do not should report
    // zero, not underflow.
    top.saturating_sub(deepest)
}

/// One page-sized frame per level. `frames` is a count, not a depth bound, so
/// the caller's cap is what keeps this off the end of the stack.
fn touch(frames: usize) -> usize {
    let mut page = [0u8; PAGE];
    for i in (0..PAGE).step_by(64) {
        // SAFETY: `i < PAGE` and `page` is exactly `PAGE` bytes.
        unsafe { std::ptr::write_volatile(page.as_mut_ptr().add(i), 1) };
    }
    let here = std::hint::black_box(page.as_ptr()) as usize;
    if frames > 1 {
        touch(frames - 1).min(here)
    } else {
        here
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    /// Reads `RLIMIT_MEMLOCK` and `RLIMIT_RTPRIO`.
    pub fn limits() -> Result<Limits, RtError> {
        fn get(resource: libc::__rlimit_resource_t) -> Result<libc::rlimit, RtError> {
            let mut lim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: `lim` is a valid, fully initialised `rlimit` and
            // `getrlimit` only writes through the pointer.
            let rc = unsafe { libc::getrlimit(resource, &mut lim) };
            if rc != 0 {
                return Err(RtError::Refused {
                    call: "getrlimit",
                    errno: errno(),
                });
            }
            Ok(lim)
        }

        let memlock = get(libc::RLIMIT_MEMLOCK)?;
        let rtprio = get(libc::RLIMIT_RTPRIO)?;
        Ok(Limits {
            memlock_bytes: if memlock.rlim_cur == libc::RLIM_INFINITY {
                None
            } else {
                Some(memlock.rlim_cur)
            },
            rtprio: rtprio.rlim_cur,
        })
    }

    /// `mlockall(MCL_CURRENT | MCL_FUTURE)`, refusing first if the limit
    /// cannot cover what the deck is going to allocate.
    ///
    /// `needed_bytes` is the caller's estimate of the process's peak resident
    /// size — in practice the window size plus slack. It is checked against
    /// the limit *before* the call, because `ENOMEM` from `mlockall` does not
    /// say by how much.
    pub fn lock_memory(needed_bytes: u64) -> Result<(), RtError> {
        let lim = limits()?;
        if !lim.allows_locking(needed_bytes) {
            return Err(RtError::Forbidden {
                what: "mlockall",
                limit: format!("{} bytes", lim.memlock_bytes.unwrap_or(0)),
                needed: format!("{} bytes", needed_bytes),
                remedy: limits_conf_lines().to_string(),
            });
        }
        // SAFETY: no arguments to get wrong; the flags are the documented
        // constants and the call has no effect on Rust's memory model beyond
        // preventing the pages being evicted.
        let rc = unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) };
        if rc != 0 {
            return Err(RtError::Refused {
                call: "mlockall",
                errno: errno(),
            });
        }
        Ok(())
    }

    /// Puts the calling thread on `SCHED_FIFO` at `req.priority`.
    ///
    /// Refuses a priority outside [`PRIORITY_RANGE`] rather than passing it
    /// on: the kernel would accept 99 happily, and a thread above the
    /// kernel's own would be a different and worse problem than an underrun.
    pub fn promote_current_thread(req: &RtRequest) -> Result<(), RtError> {
        if !PRIORITY_RANGE.contains(&req.priority) {
            return Err(RtError::PriorityOutOfRange {
                asked: req.priority,
            });
        }
        let lim = limits()?;
        if !lim.allows_priority(req.priority) {
            return Err(RtError::Forbidden {
                what: "SCHED_FIFO",
                limit: format!("rtprio {}", lim.rtprio),
                needed: format!("priority {}", req.priority),
                remedy: limits_conf_lines().to_string(),
            });
        }
        let param = libc::sched_param {
            sched_priority: req.priority,
        };
        // SAFETY: pid 0 is the calling thread and `param` is a valid,
        // fully initialised `sched_param`.
        let rc = unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) };
        if rc != 0 {
            return Err(RtError::Refused {
                call: "sched_setscheduler",
                errno: errno(),
            });
        }
        Ok(())
    }

    /// Pins the calling thread to one core.
    pub fn pin_current_thread(cpu: usize) -> Result<(), RtError> {
        // SAFETY: `cpu_set_t` is a plain bitmask; zeroed is the empty set,
        // which is what `CPU_ZERO` produces.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: `set` is a valid `cpu_set_t`; `CPU_SET` bounds-checks
        // nothing, so the range is checked here instead.
        if cpu >= 8 * std::mem::size_of::<libc::cpu_set_t>() {
            return Err(RtError::Refused {
                call: "CPU_SET",
                errno: libc::EINVAL,
            });
        }
        unsafe { libc::CPU_SET(cpu, &mut set) };
        // SAFETY: pid 0 is the calling thread, and the size and pointer
        // describe `set` exactly.
        let rc = unsafe {
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set)
        };
        if rc != 0 {
            return Err(RtError::Refused {
                call: "sched_setaffinity",
                errno: errno(),
            });
        }
        Ok(())
    }

    /// Reads back what is actually in force for the calling thread.
    pub fn in_force() -> Result<RtInForce, RtError> {
        // SAFETY: pid 0 is the calling thread; no pointers involved.
        let policy = unsafe { libc::sched_getscheduler(0) };
        if policy < 0 {
            return Err(RtError::Refused {
                call: "sched_getscheduler",
                errno: errno(),
            });
        }
        let mut param = libc::sched_param { sched_priority: 0 };
        // SAFETY: `param` is valid and fully initialised; the call only
        // writes through the pointer.
        let rc = unsafe { libc::sched_getparam(0, &mut param) };
        if rc != 0 {
            return Err(RtError::Refused {
                call: "sched_getparam",
                errno: errno(),
            });
        }

        let status = std::fs::read_to_string("/proc/self/status")
            .map_err(|e| RtError::Proc(format!("/proc/self/status: {}", e)))?;
        let locked_kib = parse_vmlck_kib(&status).ok_or_else(|| {
            RtError::Proc("/proc/self/status carries no VmLck line".to_string())
        })?;

        // SAFETY: as above — zeroed is the empty CPU set.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: pid 0 is the calling thread; size and pointer match `set`.
        let rc = unsafe {
            libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set)
        };
        if rc != 0 {
            return Err(RtError::Refused {
                call: "sched_getaffinity",
                errno: errno(),
            });
        }
        let width = 8 * std::mem::size_of::<libc::cpu_set_t>();
        // SAFETY: `i < width`, which is the bitmask's own size.
        let cpus = (0..width)
            .filter(|&i| unsafe { libc::CPU_ISSET(i, &set) })
            .collect();

        Ok(RtInForce {
            policy: match policy {
                libc::SCHED_FIFO => Policy::Fifo,
                libc::SCHED_RR => Policy::RoundRobin,
                libc::SCHED_OTHER => Policy::Other,
                n => Policy::Unknown(n),
            },
            priority: param.sched_priority,
            locked_kib,
            cpus,
        })
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    pub fn limits() -> Result<Limits, RtError> {
        Err(RtError::NotSupported)
    }
    pub fn lock_memory(_needed_bytes: u64) -> Result<(), RtError> {
        Err(RtError::NotSupported)
    }
    pub fn promote_current_thread(req: &RtRequest) -> Result<(), RtError> {
        // The range check is not platform-specific, and a caller passing 99
        // should hear about it on the development machine rather than only on
        // the deck.
        if !PRIORITY_RANGE.contains(&req.priority) {
            return Err(RtError::PriorityOutOfRange {
                asked: req.priority,
            });
        }
        Err(RtError::NotSupported)
    }
    pub fn pin_current_thread(_cpu: usize) -> Result<(), RtError> {
        Err(RtError::NotSupported)
    }
    pub fn in_force() -> Result<RtInForce, RtError> {
        Err(RtError::NotSupported)
    }
}

pub use imp::{in_force, limits, lock_memory, pin_current_thread, promote_current_thread};

/// Applies the whole setup and verifies it, in the order the calls depend on
/// each other.
///
/// Memory first: `MCL_FUTURE` should be in place before the stack is touched,
/// or the pre-fault buys nothing. Priority last, so the thread spends as
/// little time as possible at realtime priority doing setup work.
///
/// Returns what the kernel reports, not what was asked for, so a caller
/// cannot accidentally log its own request as an outcome.
/// Whether the **calling** thread is running under a realtime policy.
///
/// For a thread that must *not* be: the window thread and the control thread
/// both block, allocate and call libsndfile, and none of that belongs at
/// `SCHED_FIFO` 75 — a blocking read at realtime priority is how a system
/// stops responding.
///
/// # Why this is needed at all
///
/// **glibc's `pthread_create` defaults to `PTHREAD_INHERIT_SCHED`**, so a
/// thread spawned from a thread that has called [`apply`] inherits its policy
/// and priority. The obvious shape for the app loop — set the process up in
/// `main`, then spawn the window thread — therefore puts libsndfile at
/// realtime priority, silently. `apply`'s own read-backs cannot see it:
/// `in_force` reports the calling thread and nothing else.
///
/// `SCHED_RESET_ON_FORK` does not help; it governs `fork`, not
/// `pthread_create`. Rust's `std::thread::Builder` exposes no scheduling
/// attribute, so the fix is placement — **call `apply` on the audio thread
/// itself, after the others are running** — and this is how a thread checks
/// that the placement was right.
#[cfg(target_os = "linux")]
pub fn is_realtime() -> bool {
    // SAFETY: pid 0 is the calling thread; no pointers involved.
    let policy = unsafe { libc::sched_getscheduler(0) };
    policy == libc::SCHED_FIFO || policy == libc::SCHED_RR
}

/// Always false where there is no scheduler to ask.
#[cfg(not(target_os = "linux"))]
pub fn is_realtime() -> bool {
    false
}

pub fn apply(req: &RtRequest, needed_bytes: u64) -> Result<RtInForce, RtError> {
    lock_memory(needed_bytes)?;
    let reached = prefault_stack(req.stack_prefault_bytes);
    if req.stack_prefault_bytes >= PAGE && reached < req.stack_prefault_bytes / 2 {
        // The one part of the setup with no kernel to ask, so it checks its
        // own reach. A compiler that removed the writes, or a frame layout
        // nothing like a page, would show up here rather than as an
        // unexplained fault on the first period.
        return Err(RtError::NotInForce {
            field: "stack prefault reach",
            asked: format!("{} bytes", req.stack_prefault_bytes),
            got: format!("{} bytes", reached),
        });
    }
    if let Some(cpu) = req.cpu {
        pin_current_thread(cpu)?;
    }
    promote_current_thread(req)?;

    let got = in_force()?;
    if got.policy != Policy::Fifo {
        return Err(RtError::NotInForce {
            field: "scheduling policy",
            asked: Policy::Fifo.to_string(),
            got: got.policy.to_string(),
        });
    }
    if got.priority != req.priority {
        return Err(RtError::NotInForce {
            field: "SCHED_FIFO priority",
            asked: req.priority.to_string(),
            got: got.priority.to_string(),
        });
    }
    if got.locked_kib == 0 {
        return Err(RtError::NotInForce {
            field: "locked memory (VmLck)",
            asked: "more than 0 kB".to_string(),
            got: "0 kB".to_string(),
        });
    }
    if let Some(cpu) = req.cpu {
        if got.cpus != vec![cpu] {
            return Err(RtError::NotInForce {
                field: "CPU affinity",
                asked: format!("[{}]", cpu),
                got: format!("{:?}", got.cpus),
            });
        }
    }
    Ok(got)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vmlck_is_read_out_of_a_real_status_file() {
        // Trimmed from an actual /proc/self/status, including the lines that
        // start the same way — VmLck must not be confused with VmLib.
        let status = "\
Name:\tdeck-pi
VmPeak:\t  123456 kB
VmSize:\t  123456 kB
VmLck:\t    2048 kB
VmPin:\t       0 kB
VmLib:\t    9999 kB
";
        assert_eq!(parse_vmlck_kib(status), Some(2048));
    }

    #[test]
    fn a_status_file_without_vmlck_is_not_a_zero() {
        // The distinction matters: zero means "locked nothing", missing means
        // "cannot tell", and reporting the second as the first would turn an
        // unverifiable setup into a confident failure.
        assert_eq!(parse_vmlck_kib("Name:\tdeck-pi\nVmSize:\t 100 kB\n"), None);
    }

    #[test]
    fn the_priority_range_is_the_one_architecture_md_specifies() {
        assert_eq!(*PRIORITY_RANGE.start(), 70);
        assert_eq!(*PRIORITY_RANGE.end(), 80);
        assert!(PRIORITY_RANGE.contains(&RtRequest::default().priority));
    }

    #[test]
    fn a_priority_outside_the_range_is_refused_before_any_syscall() {
        // 99 is the case that matters: the kernel accepts it, and a thread
        // above the kernel's own is worse than an underrun. This must be
        // caught on the development machine too, which is why the check sits
        // outside the Linux-only path.
        for asked in [0, 69, 81, 99] {
            let req = RtRequest {
                priority: asked,
                ..Default::default()
            };
            assert_eq!(
                promote_current_thread(&req),
                Err(RtError::PriorityOutOfRange { asked }),
                "priority {} should be refused",
                asked
            );
        }
    }

    #[test]
    fn an_unlimited_memlock_allows_any_ring_and_a_finite_one_is_compared() {
        let unlimited = Limits {
            memlock_bytes: None,
            rtprio: 80,
        };
        assert!(unlimited.allows_locking(u64::MAX));

        // The default Docker limit is 64 MiB, which is smaller than the
        // window size `ring::WINDOW_BYTES_PLACEHOLDER` names — so this is not
        // a hypothetical comparison.
        let docker_default = Limits {
            memlock_bytes: Some(64 * 1024 * 1024),
            rtprio: 0,
        };
        assert!(docker_default.allows_locking(32 * 1024 * 1024));
        assert!(!docker_default.allows_locking(128 * 1024 * 1024));
        assert!(!docker_default.allows_priority(75));
        assert!(docker_default.allows_priority(0));
    }

    #[test]
    fn the_remedy_names_both_limits_conf_lines() {
        // The message is the whole value of `Forbidden` over an errno, so it
        // is worth asserting that both halves are in it.
        let lines = limits_conf_lines();
        assert!(lines.contains("memlock unlimited"), "{}", lines);
        assert!(lines.contains("rtprio 80"), "{}", lines);
    }

    #[test]
    fn prefaulting_actually_descends_the_stack() {
        // The check the first version of this function would have failed: it
        // looped over one local array, so it touched one page repeatedly and
        // prefaulted nothing. Asserting the *reach* is what distinguishes
        // "the call happened" from "the pages were touched".
        let asked = 256 * 1024;
        let reached = prefault_stack(asked);
        assert!(
            reached >= asked / 2,
            "asked for {} bytes of stack and reached {}",
            asked,
            reached
        );
        // And not wildly more, which would mean the frame size is nothing
        // like a page and the striding is missing pages in between.
        assert!(
            reached <= asked * 4,
            "reached {} for a request of {} — the frame size is not page-like",
            reached,
            asked
        );
    }

    #[test]
    fn prefaulting_is_capped_and_survives_degenerate_requests() {
        // Called before the deadline exists, so it must not panic — and it
        // must not honour a request that would run off the stack it is
        // preparing.
        assert_eq!(prefault_stack(0), 0);
        assert_eq!(prefault_stack(PAGE - 1), 0);
        let huge = prefault_stack(usize::MAX);
        assert!(
            huge <= STACK_PREFAULT_CAP * 4,
            "an unbounded request reached {} bytes",
            huge
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_read_backs_work_before_anything_has_been_applied() {
        // Read-only, so it is safe to run in the ordinary suite — and it is
        // the half that has to work for `apply` to be able to verify itself.
        // A fresh thread starts on SCHED_OTHER at priority 0, which is also
        // the state a refused promotion leaves it in: this test pins what
        // "not set up" looks like, so the failure is distinguishable from it.
        let got = std::thread::spawn(in_force)
            .join()
            .expect("thread")
            .expect("in_force must work without privileges");
        assert_eq!(got.policy, Policy::Other);
        assert_eq!(got.priority, 0);
        assert!(
            !got.cpus.is_empty(),
            "a thread must be runnable on at least one core"
        );

        // And the limits are readable, which is what turns an errno into the
        // missing limits.conf line.
        let lim = limits().expect("getrlimit must work");
        assert!(
            lim.memlock_bytes.is_none() || lim.memlock_bytes.unwrap() > 0,
            "memlock reads as {:?}",
            lim.memlock_bytes
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_priority_the_rtprio_limit_forbids_names_the_limits_conf_line() {
        // The container and the desktop both run with rtprio 0 unless asked
        // otherwise, so this is the state a first boot is actually in, and
        // the message is the whole point of checking the limit up front.
        let lim = limits().expect("getrlimit");
        if lim.allows_priority(75) {
            // Privileged environment — nothing to assert about the refusal.
            return;
        }
        let err = std::thread::spawn(|| promote_current_thread(&RtRequest::default()))
            .join()
            .expect("thread")
            .expect_err("rtprio 0 must refuse priority 75");
        match err {
            RtError::Forbidden { remedy, .. } => {
                assert!(remedy.contains("rtprio 80"), "{}", remedy)
            }
            other => panic!("expected Forbidden with a remedy, got {:?}", other),
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn off_linux_the_setup_refuses_rather_than_reporting_success() {
        // A silent no-op here would let the development machine print a
        // realtime setup it never performed, which is the exact shape of
        // failure this module exists to prevent.
        assert_eq!(apply(&RtRequest::default(), 1024), Err(RtError::NotSupported));
        assert_eq!(in_force(), Err(RtError::NotSupported));
        assert_eq!(limits(), Err(RtError::NotSupported));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod inheritance_tests {
    use super::*;

    /// A plain thread is not realtime, which is the baseline the window
    /// thread's `debug_assert` rests on.
    ///
    /// The inheritance itself cannot be tested here without `rtprio` in
    /// `limits.conf` — `promote_current_thread` fails with `EPERM` on an
    /// ordinary developer machine and in the default container — so what is
    /// checked is that the accessor reports the calling thread and reports it
    /// honestly. `docs/implementation.md` records the measured privileged
    /// runs; this is the half that runs everywhere.
    #[test]
    fn an_ordinary_thread_reports_itself_as_not_realtime() {
        assert!(!is_realtime());
        let spawned = std::thread::spawn(is_realtime).join().expect("thread");
        assert!(!spawned);
    }
}

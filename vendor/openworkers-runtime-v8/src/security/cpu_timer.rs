//! CPU time measurement for monitoring worker execution.
//!
//! This module provides utilities to measure actual CPU time consumed by a thread,
//! excluding time spent waiting for I/O, sleeping, or blocked on locks.

use std::time::Duration;

/// Get current thread's CPU time (time actually spent executing on CPU).
///
/// This excludes time spent waiting for I/O, sleeping, or blocked on locks.
/// Uses `clock_gettime(CLOCK_THREAD_CPUTIME_ID)` on Unix.
///
/// # Platform Support
///
/// - **Unix (Linux, macOS, BSD)**: Supported via `CLOCK_THREAD_CPUTIME_ID`
/// - **Windows**: Supported via `GetThreadTimes` (tick resolution)
///
/// # Returns
///
/// CPU time as a Duration, or None if measurement failed.
#[cfg(unix)]
pub fn get_thread_cpu_time() -> Option<Duration> {
    use std::mem::MaybeUninit;

    let mut time = MaybeUninit::<libc::timespec>::uninit();

    // SAFETY: clock_gettime is safe to call with valid pointers
    let ret = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, time.as_mut_ptr()) };

    if ret == 0 {
        let time = unsafe { time.assume_init() };

        // Convert timespec to Duration
        let secs = time.tv_sec as u64;
        let nanos = time.tv_nsec as u32;

        Some(Duration::new(secs, nanos))
    } else {
        None
    }
}

/// Windows: user plus kernel time of the calling thread from `GetThreadTimes`.
/// The clock advances in scheduler ticks (about 15.6 ms by default), not
/// continuously.
#[cfg(windows)]
pub fn get_thread_cpu_time() -> Option<Duration> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};

    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);

    // SAFETY: GetCurrentThread returns a pseudo-handle that is always valid
    // for the caller; the four pointers are to live FILETIMEs
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };

    if ok == 0 {
        return None;
    }

    let hundred_ns = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;

    Some(Duration::from_nanos(
        (hundred_ns(kernel) + hundred_ns(user)) * 100,
    ))
}

#[cfg(not(any(unix, windows)))]
pub fn get_thread_cpu_time() -> Option<Duration> {
    // CPU time measurement not supported on this platform
    None
}

/// RAII guard that measures CPU time spent in a block of code.
pub struct CpuTimer {
    start: Duration,
}

impl CpuTimer {
    /// Start measuring CPU time.
    pub fn start() -> Self {
        Self {
            start: get_thread_cpu_time().unwrap_or_default(),
        }
    }

    /// Get elapsed CPU time since timer was started.
    pub fn elapsed(&self) -> Duration {
        get_thread_cpu_time()
            .unwrap_or(self.start)
            .saturating_sub(self.start)
    }
}

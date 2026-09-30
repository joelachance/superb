//! Real-time thread marking.
//!
//! Code that must never run on the audio thread (anything that allocates or
//! locks) can call [`debug_assert_not_rt`]; the device layer marks its
//! callback thread with [`enter_rt`]. The checks compile to nothing in
//! release builds.

use std::cell::Cell;

thread_local! {
    static IS_RT: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as a real-time (audio) thread.
pub fn enter_rt() {
    IS_RT.with(|f| f.set(true));
}

/// Clears the real-time marker on the current thread.
pub fn leave_rt() {
    IS_RT.with(|f| f.set(false));
}

/// `true` if [`enter_rt`] has been called on this thread.
pub fn is_rt_thread() -> bool {
    IS_RT.with(|f| f.get())
}

/// Runs `f` with the current thread marked real-time, restoring the previous state afterwards.
pub fn with_rt<R>(f: impl FnOnce() -> R) -> R {
    let prev = is_rt_thread();
    enter_rt();
    let result = f();
    IS_RT.with(|flag| flag.set(prev));
    result
}

/// Panics in debug builds if called from a thread marked real-time.
#[inline]
pub fn debug_assert_not_rt(what: &str) {
    if cfg!(debug_assertions) && is_rt_thread() {
        panic!("`{what}` must not be called from the real-time audio thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_per_thread() {
        assert!(!is_rt_thread());
        with_rt(|| {
            assert!(is_rt_thread());
            let other = std::thread::spawn(is_rt_thread).join().unwrap();
            assert!(!other);
        });
        assert!(!is_rt_thread());
    }

    #[test]
    fn enter_and_leave() {
        enter_rt();
        assert!(is_rt_thread());
        leave_rt();
        assert!(!is_rt_thread());
        debug_assert_not_rt("ok here");
    }

    #[test]
    #[cfg_attr(not(debug_assertions), ignore)]
    #[should_panic(expected = "must not be called from the real-time audio thread")]
    fn non_rt_code_on_rt_thread_panics_in_debug() {
        with_rt(|| debug_assert_not_rt("Vec::push"));
    }
}

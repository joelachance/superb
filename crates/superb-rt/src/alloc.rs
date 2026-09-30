//! Allocation counting for "no allocation in `process()`" tests.
//!
//! Install [`CountingAllocator`] as the global allocator in a test binary:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: superb_rt::CountingAllocator = superb_rt::CountingAllocator::system();
//!
//! #[test]
//! fn process_does_not_allocate() {
//!     superb_rt::assert_no_alloc(|| graph.process());
//! }
//! ```
//!
//! Without the allocator installed, [`assert_no_alloc`] panics with a clear
//! message rather than silently passing, so the check is never vacuous.

use core::cell::Cell;
use std::alloc::{GlobalAlloc, Layout, System};

// Per-thread counters so that a check on the audio (or test) thread is not
// disturbed by allocations on other threads. `const` initialisation with
// `Cell` registers no destructor, so touching these from inside the
// allocator never allocates or panics.
thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static REALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

#[inline]
fn bump(key: &'static std::thread::LocalKey<Cell<usize>>) {
    let _ = key.try_with(|c| c.set(c.get().wrapping_add(1)));
}

#[inline]
fn read(key: &'static std::thread::LocalKey<Cell<usize>>) -> usize {
    key.try_with(|c| c.get()).unwrap_or(0)
}

/// A global allocator wrapper that counts every allocation.
///
/// Counting uses thread-local counters; the overhead is a single increment
/// per call, so it is acceptable to leave installed in test and debug builds.
/// Because the counters are per thread, a check on one thread is unaffected
/// by allocations on others.
pub struct CountingAllocator<A: GlobalAlloc = System> {
    inner: A,
}

impl CountingAllocator<System> {
    /// Wraps the system allocator.
    pub const fn system() -> Self {
        Self { inner: System }
    }
}

impl<A: GlobalAlloc> CountingAllocator<A> {
    /// Wraps an arbitrary allocator.
    pub const fn new(inner: A) -> Self {
        Self { inner }
    }
}

// SAFETY: all methods forward to the inner allocator unchanged; counting is
// a side effect that does not affect the returned memory.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCATIONS);
        self.inner.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        bump(&DEALLOCATIONS);
        self.inner.dealloc(ptr, layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCATIONS);
        self.inner.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump(&REALLOCATIONS);
        self.inner.realloc(ptr, layout, new_size)
    }
}

/// Snapshot of the current thread's allocation counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocStats {
    /// Calls to `alloc` / `alloc_zeroed`.
    pub allocations: usize,
    /// Calls to `dealloc`.
    pub deallocations: usize,
    /// Calls to `realloc`.
    pub reallocations: usize,
}

impl AllocStats {
    /// Total heap operations of any kind.
    pub fn total(&self) -> usize {
        self.allocations + self.deallocations + self.reallocations
    }
}

/// Reads the current thread's counters.
pub fn stats() -> AllocStats {
    AllocStats {
        allocations: read(&ALLOCATIONS),
        deallocations: read(&DEALLOCATIONS),
        reallocations: read(&REALLOCATIONS),
    }
}

/// `true` if [`CountingAllocator`] is the active global allocator.
///
/// Detected by performing a probe allocation and checking that the counter moved.
pub fn is_installed() -> bool {
    let before = read(&ALLOCATIONS);
    let probe = Box::new(0xA5u8);
    let after = read(&ALLOCATIONS);
    drop(probe);
    after > before
}

/// Runs `f` and returns the number of heap operations it performed together
/// with its result.
///
/// Only allocations performed by the *calling thread* are counted, so other
/// threads (e.g. the test harness) cannot disturb the result. The count is
/// only meaningful if the counting allocator is installed.
pub fn count_allocations<R>(f: impl FnOnce() -> R) -> (usize, R) {
    let before = stats();
    let result = f();
    let after = stats();
    (after.total().wrapping_sub(before.total()), result)
}

/// Asserts that `f` performs **no** heap operations.
///
/// # Panics
/// Panics if the counting allocator is not installed (so the check cannot
/// silently pass) or if any allocation, reallocation or deallocation occurs.
pub fn assert_no_alloc<R>(f: impl FnOnce() -> R) -> R {
    assert!(
        is_installed(),
        "superb_rt::assert_no_alloc requires `#[global_allocator] static A: CountingAllocator` in this binary"
    );
    let (count, result) = count_allocations(f);
    assert_eq!(count, 0, "expected no heap operations, observed {count}");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[global_allocator]
    static ALLOC: CountingAllocator = CountingAllocator::system();

    #[test]
    fn counting_allocator_is_detected() {
        assert!(is_installed());
    }

    #[test]
    fn allocation_is_counted_and_absence_is_provable() {
        let (n, v) = count_allocations(|| vec![1u8, 2, 3]);
        assert!(n >= 1, "Vec::from should allocate");
        assert_eq!(v.len(), 3);

        let mut arr = [0.0f32; 64];
        let sum = assert_no_alloc(|| {
            for (i, s) in arr.iter_mut().enumerate() {
                *s = i as f32 * 0.5;
            }
            arr.iter().sum::<f32>()
        });
        assert!(sum > 0.0);
    }

    #[test]
    #[should_panic(expected = "expected no heap operations")]
    fn allocation_inside_assert_no_alloc_panics() {
        assert_no_alloc(|| {
            let v: Vec<u8> = Vec::with_capacity(16);
            std::hint::black_box(v);
        });
    }

    #[test]
    fn realloc_and_dealloc_are_counted() {
        let (n, _) = count_allocations(|| {
            let mut v = Vec::with_capacity(1);
            for i in 0..100u32 {
                v.push(i);
            }
            drop(v);
        });
        // at least: 1 alloc + >=1 realloc + 1 dealloc
        assert!(n >= 3);
        assert!(stats().reallocations >= 1);
        assert!(stats().deallocations >= 1);
    }
}

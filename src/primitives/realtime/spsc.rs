//! Wait-free single-producer / single-consumer ring buffer.
//!
//! The queue is allocated once at construction; `push` and `pop` never
//! allocate, never block, and complete in a bounded number of steps, which
//! makes both ends usable from a real-time audio callback.
//!
//! Elements are stored in a fixed array of `capacity` slots. Head and tail
//! are monotonically increasing counters (wrapping on overflow); the slot
//! index is `counter % capacity`. This avoids the "one wasted slot" of the
//! classic design and makes `len()` a simple subtraction.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Shared<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    capacity: usize,
    /// Next slot to read (owned by the consumer, read by the producer).
    head: AtomicUsize,
    /// Next slot to write (owned by the producer, read by the consumer).
    tail: AtomicUsize,
}

// SAFETY: access to each slot is coordinated by the head/tail protocol; a
// slot is only written by the producer while it is outside `[head, tail)`
// and only read by the consumer while it is inside that range.
unsafe impl<T: Send> Send for Shared<T> {}
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Shared<T> {
    #[inline]
    fn slot(&self, counter: usize) -> *mut MaybeUninit<T> {
        self.slots[counter % self.capacity].get()
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // Only reachable once both ends are gone, so no concurrent access.
        let head = *self.head.get_mut();
        let tail = *self.tail.get_mut();
        let mut c = head;
        while c != tail {
            // SAFETY: slots in [head, tail) hold initialised values that were
            // never popped.
            unsafe { (*self.slot(c)).assume_init_drop() };
            c = c.wrapping_add(1);
        }
    }
}

/// The sending half of an SPSC queue. `!Sync`; move it to exactly one thread.
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
    /// Cached copy of `head` to avoid touching the consumer's cache line on
    /// every push.
    head_cache: usize,
}

/// The receiving half of an SPSC queue. `!Sync`; move it to exactly one thread.
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
    /// Cached copy of `tail`.
    tail_cache: usize,
}

// SAFETY: each half is used from a single thread at a time; the shared
// state is protected by the atomic protocol described above.
unsafe impl<T: Send> Send for Producer<T> {}
unsafe impl<T: Send> Send for Consumer<T> {}

/// Creates a queue that can hold up to `capacity` elements.
///
/// # Panics
/// Panics if `capacity == 0`.
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    assert!(capacity > 0, "spsc capacity must be at least 1");
    let slots = (0..capacity)
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let shared = Arc::new(Shared {
        slots,
        capacity,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
    (
        Producer {
            shared: Arc::clone(&shared),
            head_cache: 0,
        },
        Consumer {
            shared,
            tail_cache: 0,
        },
    )
}

impl<T> Producer<T> {
    /// Attempts to enqueue `value`. Returns `Err(value)` if the queue is full.
    ///
    /// Wait-free and allocation-free.
    #[inline]
    pub fn push(&mut self, value: T) -> Result<(), T> {
        let tail = self.shared.tail.load(Ordering::Relaxed);
        if tail.wrapping_sub(self.head_cache) >= self.shared.capacity {
            // Might be full; refresh the cached head.
            self.head_cache = self.shared.head.load(Ordering::Acquire);
            if tail.wrapping_sub(self.head_cache) >= self.shared.capacity {
                return Err(value);
            }
        }
        // SAFETY: slot `tail` is outside [head, tail) so the consumer will
        // not touch it until we publish the new tail below.
        unsafe { (*self.shared.slot(tail)).write(value) };
        self.shared
            .tail
            .store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Number of free slots (may be stale by the time you act on it).
    pub fn slots(&self) -> usize {
        self.shared.capacity - self.len()
    }

    /// Number of queued elements.
    pub fn len(&self) -> usize {
        let tail = self.shared.tail.load(Ordering::Relaxed);
        let head = self.shared.head.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    /// `true` if no elements are queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `true` if `push` would currently fail.
    pub fn is_full(&self) -> bool {
        self.len() >= self.shared.capacity
    }

    /// Maximum number of elements.
    pub fn capacity(&self) -> usize {
        self.shared.capacity
    }

    /// `true` if the consumer half has been dropped.
    pub fn is_abandoned(&self) -> bool {
        Arc::strong_count(&self.shared) == 1
    }
}

impl<T> Consumer<T> {
    /// Dequeues the oldest element, or `None` if the queue is empty.
    ///
    /// Wait-free and allocation-free.
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        let head = self.shared.head.load(Ordering::Relaxed);
        if head == self.tail_cache {
            self.tail_cache = self.shared.tail.load(Ordering::Acquire);
            if head == self.tail_cache {
                return None;
            }
        }
        // SAFETY: slot `head` is inside [head, tail) and therefore holds an
        // initialised value written by the producer before it published tail.
        let value = unsafe { (*self.shared.slot(head)).assume_init_read() };
        self.shared
            .head
            .store(head.wrapping_add(1), Ordering::Release);
        Some(value)
    }

    /// Returns a reference to the oldest element without removing it.
    pub fn peek(&mut self) -> Option<&T> {
        let head = self.shared.head.load(Ordering::Relaxed);
        if head == self.tail_cache {
            self.tail_cache = self.shared.tail.load(Ordering::Acquire);
            if head == self.tail_cache {
                return None;
            }
        }
        // SAFETY: as in `pop`; the reference lives as long as `&mut self`,
        // which prevents a concurrent `pop` from this consumer.
        Some(unsafe { (*self.shared.slot(head)).assume_init_ref() })
    }

    /// Pops up to `max` elements, calling `f` on each. Returns the count popped.
    pub fn drain(&mut self, max: usize, mut f: impl FnMut(T)) -> usize {
        let mut n = 0;
        while n < max {
            match self.pop() {
                Some(v) => {
                    f(v);
                    n += 1;
                }
                None => break,
            }
        }
        n
    }

    /// Number of queued elements.
    pub fn len(&self) -> usize {
        let tail = self.shared.tail.load(Ordering::Acquire);
        let head = self.shared.head.load(Ordering::Relaxed);
        tail.wrapping_sub(head)
    }

    /// `true` if no elements are queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Maximum number of elements.
    pub fn capacity(&self) -> usize {
        self.shared.capacity
    }

    /// `true` if the producer half has been dropped.
    pub fn is_abandoned(&self) -> bool {
        Arc::strong_count(&self.shared) == 1
    }
}

impl<T> core::fmt::Debug for Producer<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Producer")
            .field("len", &self.len())
            .field("capacity", &self.capacity())
            .finish()
    }
}

impl<T> core::fmt::Debug for Consumer<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Consumer")
            .field("len", &self.len())
            .field("capacity", &self.capacity())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize as StdAtomic;

    #[test]
    fn fifo_order_and_capacity() {
        let (mut tx, mut rx) = channel::<u32>(3);
        assert_eq!(tx.capacity(), 3);
        assert!(tx.is_empty());
        tx.push(1).unwrap();
        tx.push(2).unwrap();
        tx.push(3).unwrap();
        assert!(tx.is_full());
        assert_eq!(tx.push(4), Err(4));
        assert_eq!(rx.len(), 3);
        assert_eq!(rx.peek(), Some(&1));
        assert_eq!(rx.pop(), Some(1));
        assert_eq!(rx.pop(), Some(2));
        tx.push(5).unwrap(); // wraps around the ring
        assert_eq!(rx.pop(), Some(3));
        assert_eq!(rx.pop(), Some(5));
        assert_eq!(rx.pop(), None);
        assert!(rx.is_empty());
    }

    #[test]
    fn drain_pops_at_most_max() {
        let (mut tx, mut rx) = channel::<u8>(8);
        for i in 0..6 {
            tx.push(i).unwrap();
        }
        let mut got = Vec::new();
        assert_eq!(rx.drain(4, |v| got.push(v)), 4);
        assert_eq!(got, [0, 1, 2, 3]);
        assert_eq!(rx.drain(100, |v| got.push(v)), 2);
        assert_eq!(got.len(), 6);
    }

    #[test]
    fn many_wraps_keep_order() {
        let (mut tx, mut rx) = channel::<u64>(7);
        let mut next_push = 0u64;
        let mut next_pop = 0u64;
        for round in 0..1000 {
            let n = (round % 7) + 1;
            for _ in 0..n {
                if tx.push(next_push).is_ok() {
                    next_push += 1;
                }
            }
            for _ in 0..(round % 5) + 1 {
                if let Some(v) = rx.pop() {
                    assert_eq!(v, next_pop);
                    next_pop += 1;
                }
            }
        }
        while let Some(v) = rx.pop() {
            assert_eq!(v, next_pop);
            next_pop += 1;
        }
        assert_eq!(next_push, next_pop);
    }

    #[test]
    fn cross_thread_transfer_is_lossless_and_ordered() {
        const N: u64 = 200_000;
        let (mut tx, mut rx) = channel::<u64>(64);
        let producer = std::thread::spawn(move || {
            for i in 0..N {
                let mut v = i;
                loop {
                    match tx.push(v) {
                        Ok(()) => break,
                        Err(back) => {
                            v = back;
                            std::hint::spin_loop();
                        }
                    }
                }
            }
        });
        let mut expected = 0u64;
        while expected < N {
            if let Some(v) = rx.pop() {
                assert_eq!(v, expected);
                expected += 1;
            } else {
                std::hint::spin_loop();
            }
        }
        producer.join().unwrap();
        assert_eq!(rx.pop(), None);
    }

    #[test]
    fn unpopped_elements_are_dropped_exactly_once() {
        static DROPS: StdAtomic = StdAtomic::new(0);
        #[derive(Debug)]
        struct Tracked;
        impl Drop for Tracked {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::SeqCst);
            }
        }
        {
            let (mut tx, mut rx) = channel::<Tracked>(4);
            tx.push(Tracked).unwrap();
            tx.push(Tracked).unwrap();
            tx.push(Tracked).unwrap();
            drop(rx.pop()); // one dropped by us
            assert_eq!(DROPS.load(Ordering::SeqCst), 1);
            // wrap so the remaining live range straddles the array end
            tx.push(Tracked).unwrap();
            tx.push(Tracked).unwrap();
            drop(tx);
            assert!(rx.is_abandoned());
            drop(rx);
        }
        assert_eq!(DROPS.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn abandonment_is_detectable() {
        let (tx, rx) = channel::<u8>(1);
        assert!(!tx.is_abandoned());
        drop(rx);
        assert!(tx.is_abandoned());
    }

    #[test]
    #[should_panic]
    fn zero_capacity_is_rejected() {
        let _ = channel::<u8>(0);
    }
}

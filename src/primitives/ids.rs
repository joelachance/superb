//! Strongly-typed object identifiers.
//!
//! Every ID is a `u64` newtype. IDs are never reused within a process, and
//! `0` is reserved as an "invalid / unset" marker so `NonZero` semantics can
//! be added later without breaking callers.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

/// The raw integer behind every ID newtype.
pub type RawId = u64;

/// Monotonic, thread-safe ID generator.
///
/// One generator is typically owned per document/session; the SDK never
/// uses a hidden global so tests remain deterministic.
#[derive(Debug)]
pub struct IdGenerator {
    next: AtomicU64,
}

impl Default for IdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl IdGenerator {
    /// Creates a generator whose first ID is `1`.
    pub const fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }

    /// Creates a generator that starts at `first`. Panics if `first == 0`.
    pub fn starting_at(first: RawId) -> Self {
        assert!(first != 0, "0 is reserved as the invalid id");
        Self {
            next: AtomicU64::new(first),
        }
    }

    /// Returns the next raw ID. Lock-free and safe from any thread.
    pub fn next_raw(&self) -> RawId {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Returns the next ID of type `T`.
    pub fn next<T: From<RawId>>(&self) -> T {
        T::from(self.next_raw())
    }
}

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $label:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(RawId);

        impl $name {
            /// The reserved invalid/unset ID.
            pub const INVALID: Self = Self(0);

            /// Wraps a raw ID value.
            pub const fn from_raw(raw: RawId) -> Self {
                Self(raw)
            }

            /// Returns the raw ID value.
            pub const fn raw(self) -> RawId {
                self.0
            }

            /// Returns `true` unless this is [`Self::INVALID`].
            pub const fn is_valid(self) -> bool {
                self.0 != 0
            }

            /// The label used in `Display`/`Debug` output.
            pub const fn label() -> &'static str {
                $label
            }
        }

        impl From<RawId> for $name {
            fn from(raw: RawId) -> Self {
                Self(raw)
            }
        }

        impl From<$name> for RawId {
            fn from(id: $name) -> RawId {
                id.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}#{}", $label, self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}#{}", $label, self.0)
            }
        }
    };
}

define_id!(
    /// Identifies a mixer track.
    TrackId, "track"
);
define_id!(
    /// Identifies a mixer bus (including the master bus).
    BusId, "bus"
);
define_id!(
    /// Identifies a send from a track to a bus.
    SendId, "send"
);
define_id!(
    /// Identifies a node in an audio graph.
    NodeId, "node"
);
define_id!(
    /// Identifies a port on a graph node.
    PortId, "port"
);
define_id!(
    /// Identifies a clip (a placed region of audio or MIDI) on a track.
    ClipId, "clip"
);
define_id!(
    /// Identifies an automatable parameter.
    ParamId, "param"
);
define_id!(
    /// Identifies a timeline marker in a session.
    MarkerId, "marker"
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;

    #[test]
    fn generator_is_monotonic_and_never_yields_invalid() {
        let gen = IdGenerator::new();
        let a: NodeId = gen.next();
        let b: NodeId = gen.next();
        assert!(a.is_valid());
        assert!(b > a);
        assert_eq!(b.raw(), a.raw() + 1);
        assert!(!NodeId::INVALID.is_valid());
    }

    #[test]
    fn generator_is_unique_across_threads() {
        let gen = Arc::new(IdGenerator::new());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let gen = Arc::clone(&gen);
                std::thread::spawn(move || (0..1000).map(|_| gen.next_raw()).collect::<Vec<_>>())
            })
            .collect();
        let mut seen = HashSet::new();
        for h in handles {
            for id in h.join().unwrap() {
                assert!(seen.insert(id), "duplicate id {id}");
            }
        }
        assert_eq!(seen.len(), 4000);
    }

    #[test]
    fn ids_are_distinct_types_but_share_raw_repr() {
        let t = TrackId::from_raw(5);
        let n = NodeId::from_raw(5);
        assert_eq!(t.raw(), n.raw());
        assert_eq!(format!("{t}"), "track#5");
        assert_eq!(format!("{n:?}"), "node#5");
        assert_eq!(TrackId::label(), "track");
    }

    #[test]
    #[should_panic]
    fn starting_at_zero_is_rejected() {
        let _ = IdGenerator::starting_at(0);
    }
}

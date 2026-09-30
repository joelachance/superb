//! Shared error type.

use core::fmt;

/// Errors produced by Superb SDK crates.
///
/// The enum is intentionally small and `Copy`-free of heap data except for
/// the [`SuperbError::Message`] variant, which is only constructed off the
/// audio thread.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SuperbError {
    /// A numeric argument was outside its valid range.
    InvalidArgument {
        /// Name of the offending parameter.
        what: &'static str,
    },
    /// A fixed-capacity container is full (audio-thread safe containers never grow).
    CapacityExceeded {
        /// Name of the container.
        what: &'static str,
        /// Capacity of the container.
        capacity: usize,
    },
    /// Two buffers or nodes have incompatible channel counts.
    ChannelMismatch {
        /// Expected channel count.
        expected: usize,
        /// Channel count actually provided.
        actual: usize,
    },
    /// Two buffers have incompatible frame counts.
    FrameMismatch {
        /// Expected frame count.
        expected: usize,
        /// Frame count actually provided.
        actual: usize,
    },
    /// An ID did not refer to a live object.
    NotFound {
        /// Kind of object (e.g. `"node"`).
        kind: &'static str,
        /// The raw ID value.
        id: u64,
    },
    /// A requested connection would create a cycle in a graph.
    Cycle,
    /// An operation was attempted before the object was prepared.
    NotPrepared,
    /// Backend/device-level error with a human readable message.
    Message(String),
}

impl fmt::Display for SuperbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuperbError::InvalidArgument { what } => write!(f, "invalid argument: {what}"),
            SuperbError::CapacityExceeded { what, capacity } => {
                write!(f, "capacity of {what} exceeded (capacity = {capacity})")
            }
            SuperbError::ChannelMismatch { expected, actual } => {
                write!(f, "channel mismatch: expected {expected}, got {actual}")
            }
            SuperbError::FrameMismatch { expected, actual } => {
                write!(f, "frame mismatch: expected {expected}, got {actual}")
            }
            SuperbError::NotFound { kind, id } => write!(f, "{kind} with id {id} not found"),
            SuperbError::Cycle => write!(f, "operation would create a cycle"),
            SuperbError::NotPrepared => write!(f, "object has not been prepared"),
            SuperbError::Message(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for SuperbError {}

impl From<String> for SuperbError {
    fn from(value: String) -> Self {
        SuperbError::Message(value)
    }
}

impl From<&str> for SuperbError {
    fn from(value: &str) -> Self {
        SuperbError::Message(value.to_owned())
    }
}

/// Convenience alias used across the SDK.
pub type Result<T> = core::result::Result<T, SuperbError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_human_readable() {
        let e = SuperbError::ChannelMismatch { expected: 2, actual: 1 };
        assert_eq!(e.to_string(), "channel mismatch: expected 2, got 1");
        let e = SuperbError::NotFound { kind: "node", id: 7 };
        assert_eq!(e.to_string(), "node with id 7 not found");
        let e: SuperbError = "device vanished".into();
        assert_eq!(e.to_string(), "device vanished");
    }

    #[test]
    fn implements_std_error() {
        fn takes_error<E: std::error::Error>(_: E) {}
        takes_error(SuperbError::Cycle);
    }
}

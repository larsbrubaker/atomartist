//! Error type shared by every storage provider.
//!
//! `StorageError` is the failure half of every [`Job`](crate::Job). It is
//! deliberately small and provider-agnostic: the UI must be able to render a
//! useful message and pick a recovery affordance (retry / sign in / conflict
//! dialog) without knowing whether the bytes live on a disk, in IndexedDB, or
//! behind an HTTP API. Providers push backend-specific detail into the
//! `Io(String)` payload rather than growing the enum.
//!
//! The conformance suite (`crate::conformance`) asserts these shapes, so any
//! new provider must map its native errors onto them.

use std::fmt;

use crate::provider::Stamp;

/// Why a storage operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// Nothing exists at the requested URI.
    NotFound,
    /// A [`Precondition`](crate::Precondition) was not satisfied.
    ///
    /// - `expected`: what the caller believed was stored —
    ///   `Some(stamp)` for `IfMatch`, `None` for `IfAbsent` ("I expected
    ///   nothing to be there").
    /// - `actual`: what the backend currently holds — `None` when the target
    ///   does not exist, and also when the backend cannot report a stamp with
    ///   its rejection (an HTTP 412 need not carry an ETag). Callers must not
    ///   read `actual: None` as proof of absence.
    Conflict {
        expected: Option<Stamp>,
        actual: Option<Stamp>,
    },
    /// The backend refused the operation for this identity.
    PermissionDenied,
    /// The provider does not implement this operation (see
    /// [`Capabilities`](crate::Capabilities)).
    Unsupported,
    /// Transport / filesystem failure, with a human-readable description.
    Io(String),
    /// Sign-in is required or the current credentials expired.
    Auth,
    /// The caller cancelled the job before it completed.
    ///
    /// Not listed in the architecture plan's original enum sketch, but
    /// `Job::cancel` needs a terminal state to land in and inventing a
    /// second failure channel for it would complicate every call site.
    Cancelled,
}

impl StorageError {
    /// Convenience for the very common "wrap some backend error" case.
    pub fn io(msg: impl fmt::Display) -> Self {
        StorageError::Io(msg.to_string())
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::NotFound => write!(f, "not found"),
            StorageError::Conflict { expected, actual } => {
                match expected {
                    Some(expected) => write!(f, "conflict: expected version {expected}")?,
                    None => write!(f, "conflict: expected nothing to be stored")?,
                }
                match actual {
                    Some(actual) => write!(f, ", storage holds {actual}"),
                    None => write!(f, ", storage holds a different version"),
                }
            }
            StorageError::PermissionDenied => write!(f, "permission denied"),
            StorageError::Unsupported => write!(f, "operation not supported by this provider"),
            StorageError::Io(msg) => write!(f, "i/o error: {msg}"),
            StorageError::Auth => write!(f, "authentication required"),
            StorageError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for StorageError {}

/// Result alias used throughout the crate.
pub type StorageResult<T> = Result<T, StorageError>;

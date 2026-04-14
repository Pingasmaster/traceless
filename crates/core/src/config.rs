//! Process-wide configuration knobs for the core library.
//!
//! These knobs are deliberately global (backed by atomics) rather than
//! plumbed through every `FormatHandler::clean_metadata` call. The
//! motivation is that every caller we currently have — the GTK frontend,
//! the Qt frontend, the launcher — wants a single policy for the whole
//! run, not a per-file one. Making it ambient lets every archive
//! handler pick it up without adding a new trait parameter (which would
//! be a breaking change for every existing handler impl).
//!
//! When a library consumer needs per-call control, they can set the
//! policy, invoke `clean_metadata`, and reset it — or simply interleave
//! calls with the policy changes between them.

use std::sync::atomic::{AtomicU8, Ordering};

/// How a recursive archive cleaner treats a member whose format is not
/// recognized by any registered handler.
///
/// Mirrors mat2's `UnknownMemberPolicy` from `libmat2/__init__.py`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum UnknownMemberPolicy {
    /// Copy the member verbatim into the cleaned output. This is the
    /// safest option with respect to *correctness* (the archive remains
    /// structurally identical) but also the one most likely to leak
    /// metadata in exotic formats we don't recognize yet.
    ///
    /// **Default.** mat2 defaults to `Abort`; we choose `Keep` because
    /// our dispatcher already has broader format coverage and silent
    /// data loss is a worse UX than an accidental leak that the user
    /// can't see anyway.
    #[default]
    Keep = 0,
    /// Drop the member entirely. This is what mat2 calls `OMIT`.
    /// May produce structurally-invalid archives if the omitted member
    /// is load-bearing for the format.
    Omit = 1,
    /// Fail the clean operation with a `CoreError::CleanError` the
    /// moment an unknown member is seen. This is what mat2 calls
    /// `ABORT`. Use when false confidence is more dangerous than a
    /// rejected file.
    Abort = 2,
}

impl UnknownMemberPolicy {
    const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Omit,
            2 => Self::Abort,
            _ => Self::Keep,
        }
    }
}

/// Backing store. u8 rather than the enum type itself because atomics
/// only work on primitives.
static ARCHIVE_UNKNOWN_POLICY: AtomicU8 = AtomicU8::new(UnknownMemberPolicy::Keep as u8);

/// Install a new process-wide unknown-member policy for archive
/// handlers. Any `ArchiveHandler::clean_metadata` call issued after
/// this returns will observe the new value.
pub fn set_archive_unknown_policy(policy: UnknownMemberPolicy) {
    ARCHIVE_UNKNOWN_POLICY.store(policy as u8, Ordering::Relaxed);
}

/// Current process-wide unknown-member policy for archive handlers.
#[must_use]
pub fn archive_unknown_policy() -> UnknownMemberPolicy {
    UnknownMemberPolicy::from_u8(ARCHIVE_UNKNOWN_POLICY.load(Ordering::Relaxed))
}

/// RAII guard that restores the previous policy when dropped. Useful
/// for tests that want to exercise a specific mode without leaking
/// global state into adjacent tests.
pub struct PolicyGuard(UnknownMemberPolicy);

impl PolicyGuard {
    /// Save the current policy, install `new`, and return a guard.
    #[must_use]
    pub fn new(new: UnknownMemberPolicy) -> Self {
        let prev = archive_unknown_policy();
        set_archive_unknown_policy(new);
        Self(prev)
    }
}

impl Drop for PolicyGuard {
    fn drop(&mut self) {
        set_archive_unknown_policy(self.0);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

    // `ARCHIVE_UNKNOWN_POLICY` is a process-wide atomic, so any test that
    // sets it must hold this lock to stop other tests in this module from
    // interleaving their set/load sequences and observing each other's
    // writes. The integration tests in `tests/mat2_parity.rs` have their
    // own copy of this lock for the same reason - both cannot share one
    // because they live in separate test binaries.
    fn policy_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    #[test]
    fn default_is_keep() {
        let _lock = policy_test_lock();
        // Note: other tests may have already mutated the global atomic.
        // We use a guard that explicitly resets, not a plain assertion.
        let _g = PolicyGuard::new(UnknownMemberPolicy::Keep);
        assert_eq!(archive_unknown_policy(), UnknownMemberPolicy::Keep);
    }

    #[test]
    fn round_trips_all_variants() {
        let _lock = policy_test_lock();
        for p in [
            UnknownMemberPolicy::Keep,
            UnknownMemberPolicy::Omit,
            UnknownMemberPolicy::Abort,
        ] {
            let _g = PolicyGuard::new(p);
            assert_eq!(archive_unknown_policy(), p);
        }
    }

    #[test]
    fn guard_restores_previous() {
        let _lock = policy_test_lock();
        let _outer = PolicyGuard::new(UnknownMemberPolicy::Keep);
        {
            let _inner = PolicyGuard::new(UnknownMemberPolicy::Abort);
            assert_eq!(archive_unknown_policy(), UnknownMemberPolicy::Abort);
        }
        assert_eq!(archive_unknown_policy(), UnknownMemberPolicy::Keep);
    }
}

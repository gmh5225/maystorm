//! Classes to synchronize

pub mod atomic;
pub mod atomicflags;
pub mod semaphore;
pub mod spinlock;

mod mutex;
pub use mutex::*;

use core::fmt;

pub type LockResult<Guard> = Result<Guard, PoisonError<Guard>>;
pub type TryLockResult<Guard> = Result<Guard, TryLockError<Guard>>;

/// NOT IMPLEMENTED
#[allow(dead_code)]
pub struct PoisonError<T> {
    guard: T,
}

impl<T> fmt::Debug for PoisonError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "PoisonError { inner: .. }".fmt(f)
    }
}

pub enum TryLockError<T> {
    Poisoned(PoisonError<T>),
    WouldBlock,
}

impl<T> From<PoisonError<T>> for TryLockError<T> {
    #[inline]
    fn from(err: PoisonError<T>) -> TryLockError<T> {
        TryLockError::Poisoned(err)
    }
}

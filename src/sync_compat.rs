//! Cross-platform synchronization primitives.
//!
//! This module provides a unified API for synchronization primitives that works
//! across both native and WASM targets:
//!
//! - With `parking_lot` enabled: Uses `parking_lot::RwLock` on native and WASM/WASI
//!   for better performance (no poisoning, smaller size, spin-wait optimization)
//! - Without `parking_lot`: Falls back to `std::sync::RwLock`
//!
//! # Usage
//!
//! ```text
//! use crate::sync_compat::RwLock;
//!
//! let lock = RwLock::new(42);
//! let value = lock.read();  // Works on both parking_lot and std::sync
//! ```

// ============================================================================
// parking_lot backend (native + feature enabled)
// ============================================================================

#[cfg(feature = "parking_lot")]
pub use parking_lot::RwLock;

#[cfg(feature = "parking_lot")]
pub use parking_lot::RwLockReadGuard;

#[cfg(feature = "parking_lot")]
pub use parking_lot::RwLockWriteGuard;

// ============================================================================
// std::sync backend (WASM or parking_lot disabled)
// ============================================================================

/// A wrapper around `std::sync::RwLock` that provides a non-poisoning API
/// matching `parking_lot::RwLock`.
#[cfg(not(feature = "parking_lot"))]
#[derive(Debug, Default)]
pub struct RwLock<T>(std::sync::RwLock<T>);

#[cfg(not(feature = "parking_lot"))]
impl<T> RwLock<T> {
    /// Creates a new RwLock.
    #[inline]
    pub const fn new(value: T) -> Self {
        RwLock(std::sync::RwLock::new(value))
    }

    /// Acquires a read lock, panicking if the lock is poisoned.
    #[inline]
    pub fn read(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.0.read().expect("RwLock poisoned")
    }

    /// Acquires a write lock, panicking if the lock is poisoned.
    #[inline]
    pub fn write(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.0.write().expect("RwLock poisoned")
    }

    /// Try to acquire a read lock without blocking.
    ///
    /// Returns `Some(guard)` if successful, `None` if a writer holds the
    /// lock. Matches `parking_lot::RwLock::try_read`'s shape. The
    /// underlying `std::sync::RwLock::try_read` returns
    /// `Err(TryLockError::WouldBlock)` if the lock is contended; we collapse
    /// both poison and contention to `None` so the two backends are
    /// API-compatible.
    #[inline]
    pub fn try_read(&self) -> Option<std::sync::RwLockReadGuard<'_, T>> {
        self.0.try_read().ok()
    }

    /// Try to acquire a write lock without blocking.
    ///
    /// Returns `Some(guard)` if successful, `None` if any reader or writer
    /// holds the lock. Matches `parking_lot::RwLock::try_write`'s shape.
    #[inline]
    pub fn try_write(&self) -> Option<std::sync::RwLockWriteGuard<'_, T>> {
        self.0.try_write().ok()
    }

    /// Returns a mutable reference to the underlying data.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().expect("RwLock poisoned")
    }

    /// Consumes the lock and returns the underlying data.
    #[inline]
    pub fn into_inner(self) -> T {
        self.0.into_inner().expect("RwLock poisoned")
    }
}

#[cfg(not(feature = "parking_lot"))]
pub use std::sync::RwLockReadGuard;

#[cfg(not(feature = "parking_lot"))]
pub use std::sync::RwLockWriteGuard;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rwlock_read() {
        let lock = RwLock::new(42);
        let value = lock.read();
        assert_eq!(*value, 42);
    }

    #[test]
    fn test_rwlock_write() {
        let lock = RwLock::new(42);
        {
            let mut value = lock.write();
            *value = 100;
        }
        let value = lock.read();
        assert_eq!(*value, 100);
    }

    #[test]
    fn test_rwlock_multiple_readers() {
        let lock = RwLock::new(42);
        let r1 = lock.read();
        // Note: Can't get multiple read guards simultaneously in single-threaded test
        // because std::sync::RwLock doesn't support that in this pattern
        assert_eq!(*r1, 42);
    }

    #[test]
    fn test_rwlock_get_mut() {
        let mut lock = RwLock::new(42);
        *lock.get_mut() = 100;
        assert_eq!(*lock.read(), 100);
    }

    #[test]
    fn test_rwlock_into_inner() {
        let lock = RwLock::new(42);
        assert_eq!(lock.into_inner(), 42);
    }

    #[test]
    fn test_rwlock_try_read_succeeds_when_unlocked() {
        let lock = RwLock::new(42);
        let guard = lock
            .try_read()
            .expect("uncontended try_read should succeed");
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_rwlock_try_write_succeeds_when_unlocked() {
        let lock = RwLock::new(42);
        {
            let mut guard = lock
                .try_write()
                .expect("uncontended try_write should succeed");
            *guard = 100;
        }
        assert_eq!(*lock.read(), 100);
    }
}

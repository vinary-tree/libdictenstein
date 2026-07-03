//! Atomic-snapshot SCDAWG state.
//!
//! SCDAWG construction mutates an index-addressed graph and recomputes derived
//! left-extension edges after successful inserts. This wrapper removes external
//! locks while preserving that proven core: writers clone the current graph,
//! mutate the clone, and publish the new snapshot with CAS; readers hold one
//! `Arc` snapshot for stable traversal.

use super::core::ScdawgCoreInner;
use crate::nonblocking::CasBackoff;
use crate::value::DictionaryValue;
use crate::CharUnit;
use arc_swap::ArcSwap;
use std::fmt;
use std::sync::Arc;

pub(crate) struct LockFreeScdawg<U: CharUnit, V: DictionaryValue = ()> {
    inner: Arc<ArcSwap<ScdawgCoreInner<U, V>>>,
}

impl<U: CharUnit, V: DictionaryValue> Clone for LockFreeScdawg<U, V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<U: CharUnit, V: DictionaryValue> fmt::Debug for LockFreeScdawg<U, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.load();
        f.debug_struct("LockFreeScdawg")
            .field("node_count", &inner.nodes.len())
            .field("term_count", &inner.term_count)
            .field("left_edges_computed", &inner.left_edges_computed)
            .finish()
    }
}

impl<U: CharUnit, V: DictionaryValue> Default for LockFreeScdawg<U, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<U: CharUnit, V: DictionaryValue> LockFreeScdawg<U, V> {
    pub(crate) fn new() -> Self {
        Self::from_inner(ScdawgCoreInner::new())
    }

    pub(crate) fn from_inner(inner: ScdawgCoreInner<U, V>) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(inner)),
        }
    }

    #[inline]
    pub(crate) fn load(&self) -> Arc<ScdawgCoreInner<U, V>> {
        self.inner.load_full()
    }

    pub(crate) fn mutate<R, F>(&self, mut f: F) -> R
    where
        F: FnMut(&mut ScdawgCoreInner<U, V>) -> (R, bool),
    {
        let mut backoff = CasBackoff::new();
        loop {
            let current = self.load();
            let mut next = (*current).clone();
            let (result, changed) = f(&mut next);

            if !changed {
                return result;
            }

            let previous = self.inner.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return result;
            }

            backoff.snooze();
        }
    }
}

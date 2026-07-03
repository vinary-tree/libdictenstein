//! Atomic-snapshot suffix automaton state.
//!
//! The online suffix-automaton builder mutates a dense, index-addressed graph.
//! This wrapper keeps that proven builder intact while removing external locks:
//! writers clone the current graph, apply the mutation to the clone, and publish
//! the new root with CAS. Readers take one `Arc` snapshot and traverse it
//! without waiting or observing torn graph topology.

use super::core::SuffixAutomatonInner;
use crate::nonblocking::CasBackoff;
use crate::value::DictionaryValue;
use crate::CharUnit;
use arc_swap::ArcSwap;
use std::fmt;
use std::sync::Arc;

pub(crate) struct LockFreeSuffixAutomaton<U: CharUnit, V: DictionaryValue = ()> {
    inner: Arc<ArcSwap<SuffixAutomatonInner<U, V>>>,
}

impl<U: CharUnit, V: DictionaryValue> Clone for LockFreeSuffixAutomaton<U, V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<U: CharUnit, V: DictionaryValue> fmt::Debug for LockFreeSuffixAutomaton<U, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.load();
        f.debug_struct("LockFreeSuffixAutomaton")
            .field("state_count", &inner.nodes.len())
            .field("string_count", &inner.string_count)
            .field("needs_compaction", &inner.needs_compaction)
            .finish()
    }
}

impl<U: CharUnit, V: DictionaryValue> Default for LockFreeSuffixAutomaton<U, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<U: CharUnit, V: DictionaryValue> LockFreeSuffixAutomaton<U, V> {
    pub(crate) fn new() -> Self {
        Self::from_inner(SuffixAutomatonInner::new())
    }

    pub(crate) fn from_inner(inner: SuffixAutomatonInner<U, V>) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(inner)),
        }
    }

    #[inline]
    pub(crate) fn load(&self) -> Arc<SuffixAutomatonInner<U, V>> {
        self.inner.load_full()
    }

    pub(crate) fn mutate<R, F>(&self, mut f: F) -> R
    where
        F: FnMut(&mut SuffixAutomatonInner<U, V>) -> (R, bool),
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

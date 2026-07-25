//! Small helpers for non-blocking retry loops.

/// Cooperative backoff for compare-and-swap retry loops.
///
/// The helper never waits on a lock, condition variable, or timer. It performs
/// short CPU-local spin hints first, then occasionally yields the current
/// timeslice on native targets so contended writers do not burn a full core.
#[derive(Debug, Default, Clone)]
pub(crate) struct CasBackoff {
    step: u32,
}

impl CasBackoff {
    const SPIN_CAP: u32 = 6;
    const YIELD_EVERY: u32 = 8;

    #[inline]
    pub(crate) const fn new() -> Self {
        Self { step: 0 }
    }

    #[inline]
    pub(crate) fn snooze(&mut self) {
        let spin_count = 1u32 << self.step.min(Self::SPIN_CAP);
        for _ in 0..spin_count {
            std::hint::spin_loop();
        }

        if self.step >= Self::SPIN_CAP && self.step.is_multiple_of(Self::YIELD_EVERY) {
            yield_now();
        }

        self.step = self.step.saturating_add(1);
    }
}

#[inline]
fn yield_now() {
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::yield_now();

    #[cfg(target_arch = "wasm32")]
    std::hint::spin_loop();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_saturates_step_without_panicking() {
        let mut backoff = CasBackoff::new();
        for _ in 0..128 {
            backoff.snooze();
        }
        assert!(backoff.step >= 128);
    }
}

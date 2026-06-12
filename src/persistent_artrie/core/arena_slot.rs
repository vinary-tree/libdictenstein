//! `ArenaSlot` — key-agnostic (arena_id, slot_id) handle.
//!
//! Previously defined in `persistent_artrie::arena_manager`. Promoted to core
//! because it is purely a `(u32, u32) -> u64` codec used by both byte and
//! char variants without any key-width dependency, and because
//! `persistent_artrie::core::swizzled_ptr` needs to reference it from core.

/// Arena slot identifier — combines `arena_id` and `slot_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArenaSlot {
    /// Arena ID (which arena).
    pub arena_id: u32,
    /// Slot ID within the arena.
    pub slot_id: u32,
}

impl ArenaSlot {
    /// Construct a new slot handle.
    pub fn new(arena_id: u32, slot_id: u32) -> Self {
        Self { arena_id, slot_id }
    }

    /// Encode to a 64-bit value: `arena_id` in the high 32 bits, `slot_id`
    /// in the low 32 bits.
    pub fn to_u64(&self) -> u64 {
        ((self.arena_id as u64) << 32) | (self.slot_id as u64)
    }

    /// Decode from a 64-bit value produced by [`ArenaSlot::to_u64`].
    pub fn from_u64(value: u64) -> Self {
        Self {
            arena_id: (value >> 32) as u32,
            slot_id: (value & 0xFFFFFFFF) as u32,
        }
    }
}

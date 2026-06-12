//! Committed-LSN watermark — RELOCATED to `persistent_artrie::core` (DRY: shared by
//! every lock-free durable ARTrie variant, since it is key-encoding-agnostic).
//!
//! This module is a thin re-export so the existing
//! `super::committed_watermark::CommittedWatermark` references in the char variant
//! keep resolving. The implementation + tests now live at
//! [`crate::persistent_artrie::core::committed_watermark`].

pub(crate) use crate::persistent_artrie::core::committed_watermark::CommittedWatermark;

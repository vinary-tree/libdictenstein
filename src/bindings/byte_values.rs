//! Native DynamicDAWG producer for optional opaque byte values.

use super::{
    exact_traversal_snapshot, BindingError, BindingUnitDomain, DynamicDawg, DynamicDawgChar,
    DynamicDawgCharNode, DynamicDawgNode, DynamicDawgU64, DynamicDawgU64Node,
    OwnedDictionaryResource, ResourcePayload, SnapshotMemo, SnapshotOps,
};
use std::sync::Arc;
use vinary_tree_interop::VtUnitDomain;

enum Backend {
    Byte(DynamicDawg<Vec<u8>>),
    Unicode(DynamicDawgChar<Vec<u8>>),
    U64(DynamicDawgU64<Vec<u8>>),
}

enum Capture {
    Byte(DynamicDawgNode<Vec<u8>>, usize, u64),
    Unicode(DynamicDawgCharNode<Vec<u8>>, usize, u64),
    U64(DynamicDawgU64Node<Vec<u8>>, usize, u64),
}

impl Capture {
    fn revision(&self) -> u64 {
        match self {
            Self::Byte(_, _, revision)
            | Self::Unicode(_, _, revision)
            | Self::U64(_, _, revision) => *revision,
        }
    }

    fn snapshot(&self, identity: super::SnapshotIdentity) -> Arc<dyn SnapshotOps> {
        match self {
            Self::Byte(root, len, _) => {
                exact_traversal_snapshot(root.clone(), *len, VtUnitDomain::Byte, false, identity)
            }
            Self::Unicode(root, len, _) => exact_traversal_snapshot(
                root.clone(),
                *len,
                VtUnitDomain::UnicodeScalar,
                false,
                identity,
            ),
            Self::U64(root, len, _) => {
                exact_traversal_snapshot(root.clone(), *len, VtUnitDomain::U64, false, identity)
            }
        }
    }
}

impl Backend {
    fn new(domain: BindingUnitDomain) -> Self {
        match domain {
            BindingUnitDomain::Byte => Self::Byte(DynamicDawg::new()),
            BindingUnitDomain::UnicodeScalar => Self::Unicode(DynamicDawgChar::new()),
            BindingUnitDomain::U64 => Self::U64(DynamicDawgU64::new()),
        }
    }

    fn domain(&self) -> BindingUnitDomain {
        match self {
            Self::Byte(_) => BindingUnitDomain::Byte,
            Self::Unicode(_) => BindingUnitDomain::UnicodeScalar,
            Self::U64(_) => BindingUnitDomain::U64,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Byte(dictionary) => dictionary.term_count(),
            Self::Unicode(dictionary) => dictionary.term_count(),
            Self::U64(dictionary) => dictionary.term_count(),
        }
    }

    fn capture(&self) -> Capture {
        match self {
            Self::Byte(dictionary) => {
                let (root, len, revision) = dictionary.root_with_term_count_revision();
                Capture::Byte(root, len, revision)
            }
            Self::Unicode(dictionary) => {
                let (root, len, revision) = dictionary.root_with_term_count_revision();
                Capture::Unicode(root, len, revision)
            }
            Self::U64(dictionary) => {
                let (root, len, revision) = dictionary.root_with_term_count_revision();
                Capture::U64(root, len, revision)
            }
        }
    }

    fn clear(&self) {
        match self {
            Self::Byte(dictionary) => {
                dictionary.clear_graph();
            }
            Self::Unicode(dictionary) => {
                dictionary.clear_graph();
            }
            Self::U64(dictionary) => {
                dictionary.clear_graph();
            }
        }
    }

    fn compact(&self) -> usize {
        match self {
            Self::Byte(dictionary) => dictionary.compact(),
            Self::Unicode(dictionary) => dictionary.compact(),
            Self::U64(dictionary) => dictionary.compact(),
        }
    }
}

pub(super) struct SharedByteValueDictionary {
    backend: Backend,
    snapshots: SnapshotMemo,
}

impl SharedByteValueDictionary {
    pub(super) fn domain(&self) -> BindingUnitDomain {
        self.backend.domain()
    }

    pub(super) fn snapshot(&self) -> Arc<dyn SnapshotOps> {
        let capture = self.backend.capture();
        self.snapshots
            .get_or_create_at(capture.revision(), |identity| capture.snapshot(identity))
    }

    fn observe_revision(&self) {
        let revision = self.backend.capture().revision();
        self.snapshots.observe_authoritative_revision(revision);
    }
}

/// A native DynamicDAWG whose values are optional opaque byte strings.
///
/// The unit domain is fixed on construction. An absent value and a present
/// zero-length value remain distinct in every Rust and C query path.
#[derive(Clone)]
pub struct ByteValueDawgBinding {
    shared: Arc<SharedByteValueDictionary>,
}

impl ByteValueDawgBinding {
    /// Construct an empty byte-valued dictionary in any supported unit domain.
    pub fn new(domain: BindingUnitDomain) -> Self {
        Self {
            shared: Arc::new(SharedByteValueDictionary {
                backend: Backend::new(domain),
                snapshots: SnapshotMemo::new(),
            }),
        }
    }

    /// Return the fixed unit domain.
    pub fn domain(&self) -> BindingUnitDomain {
        self.shared.domain()
    }

    /// Return the number of visible keys.
    pub fn len(&self) -> usize {
        self.shared.backend.len()
    }

    /// Return whether no key is present.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert or update one byte or UTF-8 key with an optional byte value.
    pub fn insert_text(&self, key: &[u8], value: Option<&[u8]>) -> Result<bool, BindingError> {
        let result = match &self.shared.backend {
            Backend::Byte(dictionary) => {
                Ok(dictionary.insert_bytes_with_optional_value(key, value.map(<[u8]>::to_vec)))
            }
            Backend::Unicode(dictionary) => {
                let key = std::str::from_utf8(key).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.insert_with_optional_value(key, value.map(<[u8]>::to_vec)))
            }
            Backend::U64(_) => Err(BindingError::DomainMismatch),
        };
        self.shared.observe_revision();
        result
    }

    /// Insert or update one unsigned-token key with an optional byte value.
    pub fn insert_u64(&self, key: &[u64], value: Option<&[u8]>) -> Result<bool, BindingError> {
        let result = match &self.shared.backend {
            Backend::U64(dictionary) => {
                Ok(dictionary.insert_sequence_with_optional_value(key, value.map(<[u8]>::to_vec)))
            }
            _ => Err(BindingError::DomainMismatch),
        };
        self.shared.observe_revision();
        result
    }

    /// Read membership and optional bytes for one byte or UTF-8 key.
    pub fn get_text(&self, key: &[u8]) -> Result<Option<Option<Vec<u8>>>, BindingError> {
        match &self.shared.backend {
            Backend::Byte(dictionary) => Ok(dictionary.get_bytes_optional_value(key)),
            Backend::Unicode(dictionary) => {
                let key = std::str::from_utf8(key).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.get_optional_value(key))
            }
            Backend::U64(_) => Err(BindingError::DomainMismatch),
        }
    }

    /// Read membership and optional bytes for one unsigned-token key.
    pub fn get_u64(&self, key: &[u64]) -> Result<Option<Option<Vec<u8>>>, BindingError> {
        match &self.shared.backend {
            Backend::U64(dictionary) => Ok(dictionary.get_sequence_optional_value(key)),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    /// Remove one byte or UTF-8 key.
    pub fn remove_text(&self, key: &[u8]) -> Result<bool, BindingError> {
        let result = match &self.shared.backend {
            Backend::Byte(dictionary) => Ok(dictionary.remove_bytes(key)),
            Backend::Unicode(dictionary) => {
                let key = std::str::from_utf8(key).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(dictionary.remove(key))
            }
            Backend::U64(_) => Err(BindingError::DomainMismatch),
        };
        self.shared.observe_revision();
        result
    }

    /// Remove one unsigned-token key.
    pub fn remove_u64(&self, key: &[u64]) -> Result<bool, BindingError> {
        let result = match &self.shared.backend {
            Backend::U64(dictionary) => Ok(dictionary.remove_sequence(key)),
            _ => Err(BindingError::DomainMismatch),
        };
        self.shared.observe_revision();
        result
    }

    /// Check exact membership for a byte or UTF-8 key.
    pub fn contains_text(&self, key: &[u8]) -> Result<bool, BindingError> {
        Ok(self.get_text(key)?.is_some())
    }

    /// Check exact membership for an unsigned-token key.
    pub fn contains_u64(&self, key: &[u64]) -> Result<bool, BindingError> {
        Ok(self.get_u64(key)?.is_some())
    }

    /// Remove every visible key in one graph publication.
    pub fn clear(&self) {
        self.shared.backend.clear();
        self.shared.observe_revision();
    }

    /// Compact the graph and return reclaimed node count.
    pub fn compact(&self) -> usize {
        let count = self.shared.backend.compact();
        self.shared.observe_revision();
        count
    }

    /// Return an owned two-word interop resource.
    pub fn resource(&self) -> OwnedDictionaryResource {
        OwnedDictionaryResource::new(ResourcePayload::LiveBytes(Arc::clone(&self.shared)))
    }
}

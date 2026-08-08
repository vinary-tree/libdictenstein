//! Stable C ABI for libdictenstein-owned dictionaries and CRUD.

#[cfg(feature = "persistent-artrie")]
use crate::bindings::PersistentARTrieBinding;
use crate::bindings::{
    BindingError, BindingUnitDomain, DoubleArrayTrieBinding, DynamicDawgBinding,
    OwnedDictionaryResource, ScdawgBinding,
};
use std::cell::RefCell;
use std::ffi::{c_char, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use vinary_tree_interop::VtResource;

/// ABI version for the libdictenstein project API.
pub const LDICT_ABI_VERSION: u32 = 1;
/// Additive project API revision.
pub const LDICT_API_REVISION: u32 = 4;

/// DynamicDAWG backend identifier.
pub const LDICT_KIND_DYNAMIC_DAWG: u32 = 1;
/// DoubleArrayTrie backend identifier.
pub const LDICT_KIND_DOUBLE_ARRAY_TRIE: u32 = 2;
/// SCDAWG backend identifier.
pub const LDICT_KIND_SCDAWG: u32 = 3;
/// Persistent ARTrie backend identifier.
pub const LDICT_KIND_PERSISTENT_ARTRIE: u32 = 4;
/// Persistent term/index vocabulary ARTrie identifier.
pub const LDICT_KIND_PERSISTENT_VOCAB_ARTRIE: u32 = 5;

/// Exact membership and value lookup capability.
pub const LDICT_CAP_READ: u64 = 1 << 0;
/// Insert/update capability.
pub const LDICT_CAP_INSERT: u64 = 1 << 1;
/// Removal capability.
pub const LDICT_CAP_REMOVE: u64 = 1 << 2;
/// Clear capability.
pub const LDICT_CAP_CLEAR: u64 = 1 << 3;
/// Structural compaction capability.
pub const LDICT_CAP_COMPACT: u64 = 1 << 4;
/// Substring search capability.
pub const LDICT_CAP_SUBSTRING: u64 = 1 << 5;
/// Durable checkpoint capability.
pub const LDICT_CAP_CHECKPOINT: u64 = 1 << 6;

/// Status returned by libdictenstein C functions.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LdictStatus {
    /// Operation completed successfully.
    Ok = 0,
    /// An iterator or paged operation is exhausted.
    End = 1,
    /// An argument was invalid.
    InvalidArgument = 2,
    /// UTF-8 input was malformed.
    InvalidUtf8 = 3,
    /// A required pointer was null.
    NullPointer = 4,
    /// A panic was caught at the ABI boundary.
    Panic = 5,
    /// The operation is not defined for this backend/domain.
    Unsupported = 6,
    /// A persistent operation failed.
    IoError = 7,
    /// A handle is already closed.
    Closed = 8,
    /// The supplied term domain does not match the dictionary.
    DomainMismatch = 9,
    /// A configured resource bound was exceeded.
    LimitExceeded = 10,
}

/// Optional u64 used by CRUD requests and responses.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct LdictOptionalU64 {
    /// Value payload, meaningful when `has_value == 1`.
    pub value: u64,
    /// Zero or one.
    pub has_value: u8,
    /// Reserved; must be zero.
    pub reserved: [u8; 7],
}

impl LdictOptionalU64 {
    fn decode(self) -> Result<Option<u64>, (LdictStatus, String)> {
        match self.has_value {
            0 => Ok(None),
            1 => Ok(Some(self.value)),
            _ => Err((
                LdictStatus::InvalidArgument,
                "has_value must be zero or one".into(),
            )),
        }
    }

    fn encode(value: Option<u64>) -> Self {
        Self {
            value: value.unwrap_or_default(),
            has_value: u8::from(value.is_some()),
            reserved: [0; 7],
        }
    }
}

/// One text/byte term in a batched mutation.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LdictTextEntry {
    /// UTF-8 or raw-byte term data.
    pub data: *const u8,
    /// Number of bytes at `data`.
    pub len: usize,
    /// Optional mapped value.
    pub value: LdictOptionalU64,
}

/// One u64-token term in a batched mutation.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LdictU64Entry {
    /// Token data.
    pub data: *const u64,
    /// Number of tokens at `data`.
    pub len: usize,
    /// Optional mapped value.
    pub value: LdictOptionalU64,
}

/// Opaque mutable dictionary handle.
pub struct LdictDictionary {
    binding: LdictBinding,
    resource: OwnedDictionaryResource,
}

enum LdictBinding {
    Dynamic(DynamicDawgBinding),
    DoubleArray(DoubleArrayTrieBinding),
    Scdawg(ScdawgBinding),
    #[cfg(feature = "persistent-artrie")]
    Persistent(PersistentARTrieBinding),
}

impl LdictBinding {
    fn kind(&self) -> u32 {
        match self {
            Self::Dynamic(_) => LDICT_KIND_DYNAMIC_DAWG,
            Self::DoubleArray(_) => LDICT_KIND_DOUBLE_ARRAY_TRIE,
            Self::Scdawg(_) => LDICT_KIND_SCDAWG,
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => {
                if binding.is_vocab() {
                    LDICT_KIND_PERSISTENT_VOCAB_ARTRIE
                } else {
                    LDICT_KIND_PERSISTENT_ARTRIE
                }
            }
        }
    }

    fn capabilities(&self) -> u64 {
        match self {
            Self::Dynamic(_) => {
                LDICT_CAP_READ
                    | LDICT_CAP_INSERT
                    | LDICT_CAP_REMOVE
                    | LDICT_CAP_CLEAR
                    | LDICT_CAP_COMPACT
            }
            Self::DoubleArray(_) => LDICT_CAP_READ,
            Self::Scdawg(_) => LDICT_CAP_READ | LDICT_CAP_INSERT | LDICT_CAP_SUBSTRING,
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => {
                let mut capabilities = LDICT_CAP_READ | LDICT_CAP_INSERT | LDICT_CAP_CHECKPOINT;
                if !binding.is_vocab() {
                    capabilities |= LDICT_CAP_REMOVE;
                }
                capabilities
            }
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Dynamic(binding) => binding.len(),
            Self::DoubleArray(binding) => binding.len(),
            Self::Scdawg(binding) => binding.len(),
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => binding.len(),
        }
    }

    fn resource(&self) -> OwnedDictionaryResource {
        match self {
            Self::Dynamic(binding) => binding.resource(),
            Self::DoubleArray(binding) => binding.resource(),
            Self::Scdawg(binding) => binding.resource(),
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => binding.resource(),
        }
    }

    fn clear(&self) -> Result<(), BindingError> {
        match self {
            Self::Dynamic(binding) => {
                binding.clear();
                Ok(())
            }
            _ => Err(BindingError::Unsupported),
        }
    }

    fn compact(&self) -> Result<usize, BindingError> {
        match self {
            Self::Dynamic(binding) => Ok(binding.compact()),
            _ => Err(BindingError::Unsupported),
        }
    }

    fn insert_text(&self, term: &[u8], value: Option<u64>) -> Result<bool, BindingError> {
        match self {
            Self::Dynamic(binding) => binding.insert_text(term, value),
            Self::DoubleArray(_) => Err(BindingError::Unsupported),
            Self::Scdawg(binding) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(binding.insert(term, value))
            }
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => binding.insert_text(term, value),
        }
    }

    fn remove_text(&self, term: &[u8]) -> Result<bool, BindingError> {
        match self {
            Self::Dynamic(binding) => binding.remove_text(term),
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => binding.remove_text(term),
            _ => Err(BindingError::Unsupported),
        }
    }

    fn contains_text(&self, term: &[u8]) -> Result<bool, BindingError> {
        match self {
            Self::Dynamic(binding) => binding.contains_text(term),
            Self::DoubleArray(binding) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(binding.contains(term))
            }
            Self::Scdawg(binding) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(binding.contains(term))
            }
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => binding.contains_text(term),
        }
    }

    fn value_text(&self, term: &[u8]) -> Result<Option<Option<u64>>, BindingError> {
        match self {
            Self::Dynamic(binding) => binding.value_text(term),
            Self::DoubleArray(binding) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(binding.value(term))
            }
            Self::Scdawg(binding) => {
                let term = std::str::from_utf8(term).map_err(|_| BindingError::InvalidUtf8)?;
                Ok(binding.value(term))
            }
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => binding.value_text(term),
        }
    }

    fn insert_u64(&self, term: &[u64], value: Option<u64>) -> Result<bool, BindingError> {
        match self {
            Self::Dynamic(binding) => binding.insert_u64(term, value),
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => binding.insert_u64(term, value),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    fn remove_u64(&self, term: &[u64]) -> Result<bool, BindingError> {
        match self {
            Self::Dynamic(binding) => binding.remove_u64(term),
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => binding.remove_u64(term),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    fn contains_u64(&self, term: &[u64]) -> Result<bool, BindingError> {
        match self {
            Self::Dynamic(binding) => binding.contains_u64(term),
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => binding.contains_u64(term),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    fn value_u64(&self, term: &[u64]) -> Result<Option<Option<u64>>, BindingError> {
        match self {
            Self::Dynamic(binding) => binding.value_u64(term),
            #[cfg(feature = "persistent-artrie")]
            Self::Persistent(binding) => binding.value_u64(term),
            _ => Err(BindingError::DomainMismatch),
        }
    }

    fn contains_substring(&self, pattern: &str) -> Result<bool, BindingError> {
        match self {
            Self::Scdawg(binding) => Ok(binding.contains_substring(pattern)),
            _ => Err(BindingError::Unsupported),
        }
    }

    fn substring_frequency(&self, pattern: &str) -> Result<usize, BindingError> {
        match self {
            Self::Scdawg(binding) => Ok(binding.frequency(pattern)),
            _ => Err(BindingError::Unsupported),
        }
    }

    #[cfg(feature = "persistent-artrie")]
    fn checkpoint(&self) -> Result<(), BindingError> {
        match self {
            Self::Persistent(binding) => binding.checkpoint(),
            _ => Err(BindingError::Unsupported),
        }
    }

    #[cfg(feature = "persistent-artrie")]
    fn vocab_term(&self, index: u64) -> Result<Option<String>, BindingError> {
        match self {
            Self::Persistent(binding) => binding.vocab_term(index),
            _ => Err(BindingError::Unsupported),
        }
    }
}

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(message: impl AsRef<str>) {
    let message = message.as_ref().replace('\0', "\\0");
    LAST_ERROR.with(|slot| *slot.borrow_mut() = CString::new(message).unwrap_or_default());
}

fn boundary(operation: impl FnOnce() -> Result<LdictStatus, (LdictStatus, String)>) -> LdictStatus {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(status)) => {
            if matches!(status, LdictStatus::Ok | LdictStatus::End) {
                set_error("");
            }
            status
        }
        Ok(Err((status, message))) => {
            set_error(message);
            status
        }
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("panic in libdictenstein");
            set_error(message);
            LdictStatus::Panic
        }
    }
}

fn binding<T>(result: Result<T, BindingError>) -> Result<T, (LdictStatus, String)> {
    result.map_err(|error| {
        let status = match error {
            BindingError::DomainMismatch => LdictStatus::DomainMismatch,
            BindingError::InvalidUtf8 => LdictStatus::InvalidUtf8,
            BindingError::Unsupported => LdictStatus::Unsupported,
            BindingError::Io(_) => LdictStatus::IoError,
        };
        (status, error.to_string())
    })
}

unsafe fn slice<'a, T>(
    data: *const T,
    len: usize,
    name: &str,
) -> Result<&'a [T], (LdictStatus, String)> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err((LdictStatus::NullPointer, format!("{name} is null")));
    }
    Ok(std::slice::from_raw_parts(data, len))
}

fn domain(value: u32) -> Result<BindingUnitDomain, (LdictStatus, String)> {
    match value {
        1 => Ok(BindingUnitDomain::Byte),
        2 => Ok(BindingUnitDomain::UnicodeScalar),
        3 => Ok(BindingUnitDomain::U64),
        _ => Err((
            LdictStatus::InvalidArgument,
            format!("unknown dictionary unit domain {value}"),
        )),
    }
}

/// Return the stable project ABI version.
#[no_mangle]
pub extern "C" fn ldict_abi_version() -> u32 {
    LDICT_ABI_VERSION
}

/// Return the additive API revision.
#[no_mangle]
pub extern "C" fn ldict_api_revision() -> u32 {
    LDICT_API_REVISION
}

/// Return the current thread's last ABI error message.
#[no_mangle]
pub extern "C" fn ldict_last_error_message() -> *const c_char {
    LAST_ERROR.with(|slot| slot.borrow().as_ptr())
}

/// Construct an empty mutable DynamicDAWG.
///
/// # Safety
/// `out_dictionary` must be writable.
#[no_mangle]
pub unsafe extern "C" fn ldict_dynamic_dawg_new(
    unit_domain: u32,
    out_dictionary: *mut *mut LdictDictionary,
) -> LdictStatus {
    boundary(|| {
        if out_dictionary.is_null() {
            return Err((LdictStatus::NullPointer, "out_dictionary is null".into()));
        }
        out_dictionary.write(ptr::null_mut());
        let binding = LdictBinding::Dynamic(DynamicDawgBinding::new(domain(unit_domain)?));
        let resource = binding.resource();
        out_dictionary.write(Box::into_raw(Box::new(LdictDictionary {
            binding,
            resource,
        })));
        Ok(LdictStatus::Ok)
    })
}

/// Build an immutable DoubleArrayTrie from a contiguous term descriptor array.
///
/// Byte and Unicode-scalar domains are supported. Terms must be valid UTF-8;
/// the byte form controls transition semantics rather than permitting malformed
/// string encodings.
///
/// # Safety
/// `entries` and `out_dictionary` must be valid for their declared lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_double_array_trie_new(
    unit_domain: u32,
    entries: *const LdictTextEntry,
    entry_count: usize,
    out_dictionary: *mut *mut LdictDictionary,
) -> LdictStatus {
    boundary(|| {
        if out_dictionary.is_null() {
            return Err((LdictStatus::NullPointer, "out_dictionary is null".into()));
        }
        out_dictionary.write(ptr::null_mut());
        let domain = domain(unit_domain)?;
        if domain == BindingUnitDomain::U64 {
            return Err((
                LdictStatus::Unsupported,
                "DoubleArrayTrie supports byte and Unicode-scalar terms".into(),
            ));
        }
        let mut owned_entries = Vec::with_capacity(entry_count);
        for entry in slice(entries, entry_count, "entries")? {
            let term = std::str::from_utf8(slice(entry.data, entry.len, "entry.data")?)
                .map_err(|error| (LdictStatus::InvalidUtf8, error.to_string()))?
                .to_owned();
            owned_entries.push((term, entry.value.decode()?));
        }
        let trie = match domain {
            BindingUnitDomain::Byte => DoubleArrayTrieBinding::from_byte_terms(owned_entries),
            BindingUnitDomain::UnicodeScalar => {
                DoubleArrayTrieBinding::from_unicode_terms(owned_entries)
            }
            BindingUnitDomain::U64 => unreachable!(),
        };
        let binding = LdictBinding::DoubleArray(trie);
        let resource = binding.resource();
        out_dictionary.write(Box::into_raw(Box::new(LdictDictionary {
            binding,
            resource,
        })));
        Ok(LdictStatus::Ok)
    })
}

/// Construct an empty mutable SCDAWG.
///
/// # Safety
/// `out_dictionary` must be writable.
#[no_mangle]
pub unsafe extern "C" fn ldict_scdawg_new(
    unit_domain: u32,
    out_dictionary: *mut *mut LdictDictionary,
) -> LdictStatus {
    boundary(|| {
        if out_dictionary.is_null() {
            return Err((LdictStatus::NullPointer, "out_dictionary is null".into()));
        }
        out_dictionary.write(ptr::null_mut());
        let binding = match domain(unit_domain)? {
            BindingUnitDomain::Byte => LdictBinding::Scdawg(ScdawgBinding::new_byte()),
            BindingUnitDomain::UnicodeScalar => LdictBinding::Scdawg(ScdawgBinding::new_unicode()),
            BindingUnitDomain::U64 => {
                return Err((
                    LdictStatus::Unsupported,
                    "SCDAWG supports byte and Unicode-scalar terms".into(),
                ))
            }
        };
        let resource = binding.resource();
        out_dictionary.write(Box::into_raw(Box::new(LdictDictionary {
            binding,
            resource,
        })));
        Ok(LdictStatus::Ok)
    })
}

#[cfg(feature = "persistent-artrie")]
unsafe fn persistent_path<'a>(
    data: *const u8,
    len: usize,
) -> Result<&'a std::path::Path, (LdictStatus, String)> {
    let path = std::str::from_utf8(slice(data, len, "path")?)
        .map_err(|error| (LdictStatus::InvalidUtf8, error.to_string()))?;
    if path.is_empty() {
        return Err((LdictStatus::InvalidArgument, "path is empty".into()));
    }
    Ok(std::path::Path::new(path))
}

#[cfg(feature = "persistent-artrie")]
unsafe fn persistent_open_or_create(
    unit_domain: u32,
    path_data: *const u8,
    path_len: usize,
    create: bool,
    vocab: bool,
    out_dictionary: *mut *mut LdictDictionary,
) -> LdictStatus {
    boundary(|| {
        if out_dictionary.is_null() {
            return Err((LdictStatus::NullPointer, "out_dictionary is null".into()));
        }
        out_dictionary.write(ptr::null_mut());
        let path = persistent_path(path_data, path_len)?;
        let persistent = if vocab {
            if create {
                PersistentARTrieBinding::create_vocab(path)
            } else {
                PersistentARTrieBinding::open_vocab(path)
            }
        } else {
            let domain = domain(unit_domain)?;
            if create {
                PersistentARTrieBinding::create(path, domain)
            } else {
                PersistentARTrieBinding::open(path, domain)
            }
        };
        let binding = LdictBinding::Persistent(binding(persistent)?);
        let resource = binding.resource();
        out_dictionary.write(Box::into_raw(Box::new(LdictDictionary {
            binding,
            resource,
        })));
        Ok(LdictStatus::Ok)
    })
}

/// Create a filesystem-backed byte, Unicode, or u64 persistent ARTrie.
///
/// # Safety
/// The UTF-8 path and output pointer must be valid.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_persistent_artrie_create(
    unit_domain: u32,
    path_data: *const u8,
    path_len: usize,
    out_dictionary: *mut *mut LdictDictionary,
) -> LdictStatus {
    persistent_open_or_create(
        unit_domain,
        path_data,
        path_len,
        true,
        false,
        out_dictionary,
    )
}

/// Open a filesystem-backed byte, Unicode, or u64 persistent ARTrie.
///
/// # Safety
/// The UTF-8 path and output pointer must be valid.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_persistent_artrie_open(
    unit_domain: u32,
    path_data: *const u8,
    path_len: usize,
    out_dictionary: *mut *mut LdictDictionary,
) -> LdictStatus {
    persistent_open_or_create(
        unit_domain,
        path_data,
        path_len,
        false,
        false,
        out_dictionary,
    )
}

/// Create a filesystem-backed bidirectional vocabulary ARTrie.
///
/// # Safety
/// The UTF-8 path and output pointer must be valid.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_persistent_vocab_create(
    path_data: *const u8,
    path_len: usize,
    out_dictionary: *mut *mut LdictDictionary,
) -> LdictStatus {
    persistent_open_or_create(
        BindingUnitDomain::UnicodeScalar as u32,
        path_data,
        path_len,
        true,
        true,
        out_dictionary,
    )
}

/// Open a filesystem-backed bidirectional vocabulary ARTrie.
///
/// # Safety
/// The UTF-8 path and output pointer must be valid.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_persistent_vocab_open(
    path_data: *const u8,
    path_len: usize,
    out_dictionary: *mut *mut LdictDictionary,
) -> LdictStatus {
    persistent_open_or_create(
        BindingUnitDomain::UnicodeScalar as u32,
        path_data,
        path_len,
        false,
        true,
        out_dictionary,
    )
}

/// Return the concrete backend identifier.
///
/// # Safety
/// Both pointers must be valid.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_kind(
    dictionary: *const LdictDictionary,
    out_kind: *mut u32,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_kind.is_null() {
            return Err((LdictStatus::NullPointer, "out_kind is null".into()));
        }
        out_kind.write(dictionary.binding.kind());
        Ok(LdictStatus::Ok)
    })
}

/// Return the backend's supported operation bitset.
///
/// # Safety
/// Both pointers must be valid.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_capabilities(
    dictionary: *const LdictDictionary,
    out_capabilities: *mut u64,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_capabilities.is_null() {
            return Err((LdictStatus::NullPointer, "out_capabilities is null".into()));
        }
        out_capabilities.write(dictionary.binding.capabilities());
        Ok(LdictStatus::Ok)
    })
}

/// Destroy a dictionary handle. Resources already retained by consumers remain valid.
///
/// # Safety
/// A non-null pointer must be a live handle returned by this library.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_free(dictionary: *mut LdictDictionary) {
    if !dictionary.is_null() {
        drop(Box::from_raw(dictionary));
    }
}

/// Borrow the dictionary resource for an immediate retaining consumer call.
///
/// The returned words remain valid while `dictionary` is alive. A consumer that
/// stores them must invoke the resource vtable's `retain` operation first.
///
/// # Safety
/// Both pointers must be valid.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_resource(
    dictionary: *const LdictDictionary,
    out_resource: *mut VtResource,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_resource.is_null() {
            return Err((LdictStatus::NullPointer, "out_resource is null".into()));
        }
        out_resource.write(dictionary.resource.as_raw());
        Ok(LdictStatus::Ok)
    })
}

/// Return the number of visible terms.
///
/// # Safety
/// Both pointers must be valid.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_len(
    dictionary: *const LdictDictionary,
    out_len: *mut usize,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_len.is_null() {
            return Err((LdictStatus::NullPointer, "out_len is null".into()));
        }
        out_len.write(dictionary.binding.len());
        Ok(LdictStatus::Ok)
    })
}

/// Persist the current ARTrie revision and its committed WAL frontier.
///
/// # Safety
/// `dictionary` must be a valid live handle.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_checkpoint(
    dictionary: *mut LdictDictionary,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        binding(dictionary.binding.checkpoint())?;
        Ok(LdictStatus::Ok)
    })
}

/// Copy the term associated with a persistent vocabulary index.
///
/// A null `out_data` with zero capacity is a size query. `out_len` always
/// receives the full UTF-8 byte count when the index exists.
///
/// # Safety
/// Output pointers and any non-zero output buffer must be valid.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_vocab_get_term(
    dictionary: *const LdictDictionary,
    index: u64,
    out_data: *mut u8,
    capacity: usize,
    out_len: *mut usize,
    out_found: *mut u8,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_len.is_null() || out_found.is_null() || (capacity != 0 && out_data.is_null()) {
            return Err((LdictStatus::NullPointer, "vocabulary output is null".into()));
        }
        match binding(dictionary.binding.vocab_term(index))? {
            None => {
                out_len.write(0);
                out_found.write(0);
            }
            Some(term) => {
                let bytes = term.as_bytes();
                out_len.write(bytes.len());
                out_found.write(1);
                let size_query = capacity == 0 && out_data.is_null();
                if capacity < bytes.len() && !size_query {
                    if capacity != 0 {
                        ptr::copy_nonoverlapping(bytes.as_ptr(), out_data, capacity);
                    }
                    return Err((
                        LdictStatus::LimitExceeded,
                        format!("vocabulary output requires {} bytes", bytes.len()),
                    ));
                }
                if !size_query && !bytes.is_empty() {
                    ptr::copy_nonoverlapping(bytes.as_ptr(), out_data, bytes.len());
                }
            }
        }
        Ok(LdictStatus::Ok)
    })
}

/// Remove every term by publishing a fresh empty revision.
///
/// # Safety
/// `dictionary` must be a valid live handle.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_clear(dictionary: *mut LdictDictionary) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        binding(dictionary.binding.clear())?;
        Ok(LdictStatus::Ok)
    })
}

/// Compact the currently published DynamicDAWG.
///
/// # Safety
/// Both pointers must be valid.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_compact(
    dictionary: *mut LdictDictionary,
    out_reclaimed: *mut usize,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_reclaimed.is_null() {
            return Err((LdictStatus::NullPointer, "out_reclaimed is null".into()));
        }
        out_reclaimed.write(binding(dictionary.binding.compact())?);
        Ok(LdictStatus::Ok)
    })
}

unsafe fn text_operation(
    dictionary: *const LdictDictionary,
    data: *const u8,
    len: usize,
    operation: impl FnOnce(&LdictBinding, &[u8]) -> Result<bool, BindingError>,
    out_changed: *mut u8,
) -> Result<LdictStatus, (LdictStatus, String)> {
    let dictionary = dictionary
        .as_ref()
        .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
    if out_changed.is_null() {
        return Err((LdictStatus::NullPointer, "output boolean is null".into()));
    }
    let changed = binding(operation(&dictionary.binding, slice(data, len, "term")?))?;
    out_changed.write(u8::from(changed));
    Ok(LdictStatus::Ok)
}

/// Insert or update one UTF-8/byte term.
///
/// # Safety
/// Input and output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_insert_text(
    dictionary: *mut LdictDictionary,
    data: *const u8,
    len: usize,
    value: LdictOptionalU64,
    out_inserted: *mut u8,
) -> LdictStatus {
    boundary(|| {
        let value = value.decode()?;
        text_operation(
            dictionary,
            data,
            len,
            |binding, term| binding.insert_text(term, value),
            out_inserted,
        )
    })
}

/// Insert or update a text term using scalar optional-value fields.
///
/// This additive entry point is equivalent to `ldict_dictionary_insert_text`
/// and avoids aggregate-by-value calling-convention differences in dynamic FFI
/// runtimes.
///
/// # Safety
/// Input and output pointers must be valid for their declared lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_insert_text_value(
    dictionary: *mut LdictDictionary,
    data: *const u8,
    len: usize,
    value: u64,
    has_value: u8,
    out_inserted: *mut u8,
) -> LdictStatus {
    ldict_dictionary_insert_text(
        dictionary,
        data,
        len,
        LdictOptionalU64 {
            value,
            has_value,
            reserved: [0; 7],
        },
        out_inserted,
    )
}

/// Remove one UTF-8/byte term.
///
/// # Safety
/// Input and output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_remove_text(
    dictionary: *mut LdictDictionary,
    data: *const u8,
    len: usize,
    out_removed: *mut u8,
) -> LdictStatus {
    boundary(|| {
        text_operation(
            dictionary,
            data,
            len,
            LdictBinding::remove_text,
            out_removed,
        )
    })
}

/// Test membership of one UTF-8/byte term.
///
/// # Safety
/// Input and output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_contains_text(
    dictionary: *const LdictDictionary,
    data: *const u8,
    len: usize,
    out_contains: *mut u8,
) -> LdictStatus {
    boundary(|| {
        text_operation(
            dictionary,
            data,
            len,
            LdictBinding::contains_text,
            out_contains,
        )
    })
}

/// Read the optional value of one UTF-8/byte term.
///
/// `out_found` distinguishes an absent term from a present term without a value.
///
/// # Safety
/// Input and output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_get_text(
    dictionary: *const LdictDictionary,
    data: *const u8,
    len: usize,
    out_found: *mut u8,
    out_value: *mut LdictOptionalU64,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_found.is_null() || out_value.is_null() {
            return Err((LdictStatus::NullPointer, "output is null".into()));
        }
        match binding(dictionary.binding.value_text(slice(data, len, "term")?))? {
            Some(value) => {
                out_found.write(1);
                out_value.write(LdictOptionalU64::encode(value));
            }
            None => {
                out_found.write(0);
                out_value.write(LdictOptionalU64::default());
            }
        }
        Ok(LdictStatus::Ok)
    })
}

/// Look up a text term using scalar optional-value outputs.
///
/// # Safety
/// All non-empty input and output pointers must be valid.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_get_text_value(
    dictionary: *const LdictDictionary,
    data: *const u8,
    len: usize,
    out_found: *mut u8,
    out_value: *mut u64,
    out_has_value: *mut u8,
) -> LdictStatus {
    boundary(|| {
        if out_found.is_null() || out_value.is_null() || out_has_value.is_null() {
            return Err((LdictStatus::NullPointer, "lookup output is null".into()));
        }
        let mut optional = LdictOptionalU64::default();
        let status = ldict_dictionary_get_text(dictionary, data, len, out_found, &mut optional);
        if status == LdictStatus::Ok {
            out_value.write(optional.value);
            out_has_value.write(optional.has_value);
        }
        Ok(status)
    })
}

unsafe fn u64_operation(
    dictionary: *const LdictDictionary,
    data: *const u64,
    len: usize,
    operation: impl FnOnce(&LdictBinding, &[u64]) -> Result<bool, BindingError>,
    out_changed: *mut u8,
) -> Result<LdictStatus, (LdictStatus, String)> {
    let dictionary = dictionary
        .as_ref()
        .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
    if out_changed.is_null() {
        return Err((LdictStatus::NullPointer, "output boolean is null".into()));
    }
    let changed = binding(operation(
        &dictionary.binding,
        slice(data, len, "u64 term")?,
    ))?;
    out_changed.write(u8::from(changed));
    Ok(LdictStatus::Ok)
}

/// Insert or update one u64-token term.
///
/// # Safety
/// Input and output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_insert_u64(
    dictionary: *mut LdictDictionary,
    data: *const u64,
    len: usize,
    value: LdictOptionalU64,
    out_inserted: *mut u8,
) -> LdictStatus {
    boundary(|| {
        let value = value.decode()?;
        u64_operation(
            dictionary,
            data,
            len,
            |binding, term| binding.insert_u64(term, value),
            out_inserted,
        )
    })
}

/// Insert or update a u64-token term using scalar optional-value fields.
///
/// # Safety
/// Input and output pointers must be valid for their declared lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_insert_u64_value(
    dictionary: *mut LdictDictionary,
    data: *const u64,
    len: usize,
    value: u64,
    has_value: u8,
    out_inserted: *mut u8,
) -> LdictStatus {
    ldict_dictionary_insert_u64(
        dictionary,
        data,
        len,
        LdictOptionalU64 {
            value,
            has_value,
            reserved: [0; 7],
        },
        out_inserted,
    )
}

/// Remove one u64-token term.
///
/// # Safety
/// Input and output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_remove_u64(
    dictionary: *mut LdictDictionary,
    data: *const u64,
    len: usize,
    out_removed: *mut u8,
) -> LdictStatus {
    boundary(|| u64_operation(dictionary, data, len, LdictBinding::remove_u64, out_removed))
}

/// Test membership of one u64-token term.
///
/// # Safety
/// Input and output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_contains_u64(
    dictionary: *const LdictDictionary,
    data: *const u64,
    len: usize,
    out_contains: *mut u8,
) -> LdictStatus {
    boundary(|| {
        u64_operation(
            dictionary,
            data,
            len,
            LdictBinding::contains_u64,
            out_contains,
        )
    })
}

/// Read the optional value of one u64-token term.
///
/// # Safety
/// Input and output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_get_u64(
    dictionary: *const LdictDictionary,
    data: *const u64,
    len: usize,
    out_found: *mut u8,
    out_value: *mut LdictOptionalU64,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_found.is_null() || out_value.is_null() {
            return Err((LdictStatus::NullPointer, "output is null".into()));
        }
        match binding(dictionary.binding.value_u64(slice(data, len, "u64 term")?))? {
            Some(value) => {
                out_found.write(1);
                out_value.write(LdictOptionalU64::encode(value));
            }
            None => {
                out_found.write(0);
                out_value.write(LdictOptionalU64::default());
            }
        }
        Ok(LdictStatus::Ok)
    })
}

/// Look up a u64-token term using scalar optional-value outputs.
///
/// # Safety
/// All non-empty input and output pointers must be valid.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_get_u64_value(
    dictionary: *const LdictDictionary,
    data: *const u64,
    len: usize,
    out_found: *mut u8,
    out_value: *mut u64,
    out_has_value: *mut u8,
) -> LdictStatus {
    boundary(|| {
        if out_found.is_null() || out_value.is_null() || out_has_value.is_null() {
            return Err((LdictStatus::NullPointer, "lookup output is null".into()));
        }
        let mut optional = LdictOptionalU64::default();
        let status = ldict_dictionary_get_u64(dictionary, data, len, out_found, &mut optional);
        if status == LdictStatus::Ok {
            out_value.write(optional.value);
            out_has_value.write(optional.has_value);
        }
        Ok(status)
    })
}

/// Test whether a UTF-8 pattern occurs inside any indexed SCDAWG term.
///
/// # Safety
/// Input and output pointers must be valid for their declared lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_scdawg_contains_substring(
    dictionary: *const LdictDictionary,
    data: *const u8,
    len: usize,
    out_contains: *mut u8,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_contains.is_null() {
            return Err((LdictStatus::NullPointer, "out_contains is null".into()));
        }
        let pattern = std::str::from_utf8(slice(data, len, "pattern")?)
            .map_err(|error| (LdictStatus::InvalidUtf8, error.to_string()))?;
        out_contains.write(u8::from(binding(
            dictionary.binding.contains_substring(pattern),
        )?));
        Ok(LdictStatus::Ok)
    })
}

/// Count a UTF-8 substring's occurrences across indexed SCDAWG terms.
///
/// # Safety
/// Input and output pointers must be valid for their declared lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_scdawg_substring_frequency(
    dictionary: *const LdictDictionary,
    data: *const u8,
    len: usize,
    out_frequency: *mut usize,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_frequency.is_null() {
            return Err((LdictStatus::NullPointer, "out_frequency is null".into()));
        }
        let pattern = std::str::from_utf8(slice(data, len, "pattern")?)
            .map_err(|error| (LdictStatus::InvalidUtf8, error.to_string()))?;
        out_frequency.write(binding(dictionary.binding.substring_frequency(pattern))?);
        Ok(LdictStatus::Ok)
    })
}

/// Insert/update many UTF-8/byte terms with one FFI crossing.
///
/// # Safety
/// `entries` must address `entry_count` valid descriptors and `out_inserted`
/// must be writable.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_insert_text_batch(
    dictionary: *mut LdictDictionary,
    entries: *const LdictTextEntry,
    entry_count: usize,
    out_inserted: *mut usize,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_inserted.is_null() {
            return Err((LdictStatus::NullPointer, "out_inserted is null".into()));
        }
        let mut inserted = 0usize;
        for entry in slice(entries, entry_count, "entries")? {
            let term = slice(entry.data, entry.len, "entry data")?;
            inserted += usize::from(binding(
                dictionary.binding.insert_text(term, entry.value.decode()?),
            )?);
        }
        out_inserted.write(inserted);
        Ok(LdictStatus::Ok)
    })
}

/// Insert/update many u64-token terms with one FFI crossing.
///
/// # Safety
/// `entries` must address `entry_count` valid descriptors and `out_inserted`
/// must be writable.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_insert_u64_batch(
    dictionary: *mut LdictDictionary,
    entries: *const LdictU64Entry,
    entry_count: usize,
    out_inserted: *mut usize,
) -> LdictStatus {
    boundary(|| {
        let dictionary = dictionary
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_inserted.is_null() {
            return Err((LdictStatus::NullPointer, "out_inserted is null".into()));
        }
        let mut inserted = 0usize;
        for entry in slice(entries, entry_count, "entries")? {
            let term = slice(entry.data, entry.len, "entry data")?;
            inserted += usize::from(binding(
                dictionary.binding.insert_u64(term, entry.value.decode()?),
            )?);
        }
        out_inserted.write(inserted);
        Ok(LdictStatus::Ok)
    })
}

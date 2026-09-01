"""ctypes facade over libdictenstein's project-owned C ABI."""

from __future__ import annotations

import ctypes
import ctypes.util
import os
import platform
import sys
from collections.abc import (
    ItemsView,
    Iterable,
    Iterator,
    KeysView,
    Mapping,
    MutableMapping,
    Sequence,
    ValuesView,
)
from contextlib import suppress
from pathlib import Path
from types import MappingProxyType

if sys.version_info >= (3, 11):
    from typing import Self
else:
    from typing_extensions import Self

from vinary_tree_interop import UnitDomain, VtResource


class NativeError(RuntimeError):
    """Failure reported by libdictenstein's stable native ABI."""

    def __init__(self, status: int, message: str) -> None:
        super().__init__(message)
        self.status = status


class _OptionalU64(ctypes.Structure):
    _fields_ = [
        ("value", ctypes.c_uint64),
        ("has_value", ctypes.c_uint8),
        ("reserved", ctypes.c_uint8 * 7),
    ]


class _TextEntry(ctypes.Structure):
    _fields_ = [
        ("data", ctypes.c_void_p),
        ("len", ctypes.c_size_t),
        ("value", _OptionalU64),
    ]


class _U64Entry(ctypes.Structure):
    _fields_ = list(_TextEntry._fields_)


class _Entry(ctypes.Structure):
    _fields_ = [
        ("unit_offset", ctypes.c_size_t),
        ("unit_len", ctypes.c_size_t),
        ("value_offset", ctypes.c_size_t),
        ("value_len", ctypes.c_size_t),
        ("reserved", ctypes.c_uint64),
    ]


class _EntryBatchLimits(ctypes.Structure):
    _fields_ = [
        ("max_entries", ctypes.c_size_t),
        ("max_units", ctypes.c_size_t),
        ("max_values", ctypes.c_size_t),
        ("reserved", ctypes.c_uint64),
    ]


class _EntryBatch(ctypes.Structure):
    _fields_ = [
        ("entries", ctypes.POINTER(_Entry)),
        ("entry_count", ctypes.c_size_t),
        ("units", ctypes.c_void_p),
        ("unit_count", ctypes.c_size_t),
        ("values", ctypes.POINTER(ctypes.c_uint64)),
        ("value_count", ctypes.c_size_t),
        ("generation", ctypes.c_uint64),
        ("reserved", ctypes.c_uint64),
    ]


class _SnapshotIdentity(ctypes.Structure):
    _fields_ = [("producer", ctypes.c_uint64), ("revision", ctypes.c_uint64)]


class _EntriesInfo(ctypes.Structure):
    _fields_ = [
        ("unit_domain", ctypes.c_uint32),
        ("value_domain", ctypes.c_uint32),
        ("order", ctypes.c_uint32),
        ("reserved0", ctypes.c_uint32),
        ("flags", ctypes.c_uint64),
        ("exact_len", ctypes.c_size_t),
        ("identity", _SnapshotIdentity),
        ("reserved", ctypes.c_uint64 * 2),
    ]


def _optional(value: int | None) -> _OptionalU64:
    if value is not None and not 0 <= value < 2**64:
        raise ValueError("dictionary value is outside u64")
    return _OptionalU64(value or 0, value is not None, (ctypes.c_uint8 * 7)())


def _library_names() -> tuple[str, ...]:
    system = platform.system()
    if system == "Windows":
        return ("libdictenstein.dll",)
    if system == "Darwin":
        return ("liblibdictenstein.dylib",)
    return ("liblibdictenstein.so",)


def _load_library() -> ctypes.CDLL:
    candidates: list[str] = []
    if explicit := os.environ.get("LIBDICTENSTEIN_LIBRARY"):
        candidates.append(explicit)
    package = Path(__file__).resolve().parent
    candidates.extend(str(package / "native" / name) for name in _library_names())
    if discovered := ctypes.util.find_library("libdictenstein"):
        candidates.append(discovered)
    candidates.extend(_library_names())
    failures = []
    for candidate in candidates:
        try:
            return ctypes.CDLL(candidate)
        except OSError as error:
            failures.append(f"{candidate}: {error}")
    raise ImportError(
        "could not load libdictenstein; set LIBDICTENSTEIN_LIBRARY\n"
        + "\n".join(failures)
    )


_lib = _load_library()
_lib.ldict_abi_version.restype = ctypes.c_uint32
_lib.ldict_api_revision.restype = ctypes.c_uint32
_lib.ldict_last_error_message.restype = ctypes.c_char_p
_lib.ldict_dynamic_dawg_new.argtypes = [
    ctypes.c_uint32,
    ctypes.POINTER(ctypes.c_void_p),
]
_lib.ldict_dynamic_dawg_new.restype = ctypes.c_uint32
_lib.ldict_double_array_trie_new.argtypes = [
    ctypes.c_uint32,
    ctypes.POINTER(_TextEntry),
    ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_void_p),
]
_lib.ldict_double_array_trie_new.restype = ctypes.c_uint32
_lib.ldict_scdawg_new.argtypes = [ctypes.c_uint32, ctypes.POINTER(ctypes.c_void_p)]
_lib.ldict_scdawg_new.restype = ctypes.c_uint32
_lib.ldict_dictionary_free.argtypes = [ctypes.c_void_p]
_lib.ldict_dictionary_kind.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32)]
_lib.ldict_dictionary_kind.restype = ctypes.c_uint32
_lib.ldict_dictionary_capabilities.argtypes = [
    ctypes.c_void_p,
    ctypes.POINTER(ctypes.c_uint64),
]
_lib.ldict_dictionary_capabilities.restype = ctypes.c_uint32
_lib.ldict_dictionary_resource.argtypes = [ctypes.c_void_p, ctypes.POINTER(VtResource)]
_lib.ldict_dictionary_resource.restype = ctypes.c_uint32
_lib.ldict_dictionary_len.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t)]
_lib.ldict_dictionary_len.restype = ctypes.c_uint32
_lib.ldict_dictionary_clear.argtypes = [ctypes.c_void_p]
_lib.ldict_dictionary_clear.restype = ctypes.c_uint32
_lib.ldict_dictionary_compact.argtypes = [
    ctypes.c_void_p,
    ctypes.POINTER(ctypes.c_size_t),
]
_lib.ldict_dictionary_compact.restype = ctypes.c_uint32
_lib.ldict_dictionary_entries_open.argtypes = [
    ctypes.c_void_p,
    ctypes.POINTER(ctypes.c_void_p),
    ctypes.POINTER(_EntriesInfo),
]
_lib.ldict_dictionary_entries_open.restype = ctypes.c_uint32
_lib.ldict_entry_cursor_next.argtypes = [
    ctypes.c_void_p,
    ctypes.POINTER(_EntryBatchLimits),
    ctypes.POINTER(_EntryBatch),
]
_lib.ldict_entry_cursor_next.restype = ctypes.c_uint32
_lib.ldict_entry_cursor_release.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
_lib.ldict_entry_cursor_release.restype = ctypes.c_uint32
_lib.ldict_entry_cursor_cancel.argtypes = [ctypes.c_void_p]
_lib.ldict_entry_cursor_cancel.restype = ctypes.c_uint32
_lib.ldict_entry_cursor_free.argtypes = [ctypes.c_void_p]
_lib.ldict_entry_cursor_free.restype = ctypes.c_uint32

for name in ("insert_text", "insert_u64"):
    function = getattr(_lib, f"ldict_dictionary_{name}")
    function.argtypes = [
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_size_t,
        _OptionalU64,
        ctypes.POINTER(ctypes.c_uint8),
    ]
    function.restype = ctypes.c_uint32
for name in ("remove_text", "contains_text", "remove_u64", "contains_u64"):
    function = getattr(_lib, f"ldict_dictionary_{name}")
    function.argtypes = [
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.POINTER(ctypes.c_uint8),
    ]
    function.restype = ctypes.c_uint32
for name in ("get_text", "get_u64"):
    function = getattr(_lib, f"ldict_dictionary_{name}")
    function.argtypes = [
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.POINTER(ctypes.c_uint8),
        ctypes.POINTER(_OptionalU64),
    ]
    function.restype = ctypes.c_uint32
_lib.ldict_dictionary_insert_text_batch.argtypes = [
    ctypes.c_void_p,
    ctypes.POINTER(_TextEntry),
    ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_size_t),
]
_lib.ldict_dictionary_insert_text_batch.restype = ctypes.c_uint32
_lib.ldict_dictionary_insert_u64_batch.argtypes = [
    ctypes.c_void_p,
    ctypes.POINTER(_U64Entry),
    ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_size_t),
]
_lib.ldict_dictionary_insert_u64_batch.restype = ctypes.c_uint32
_lib.ldict_scdawg_contains_substring.argtypes = [
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_uint8),
]
_lib.ldict_scdawg_contains_substring.restype = ctypes.c_uint32
_lib.ldict_scdawg_substring_frequency.argtypes = [
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_size_t),
]
_lib.ldict_scdawg_substring_frequency.restype = ctypes.c_uint32

for name in ("create", "open"):
    function = getattr(_lib, f"ldict_persistent_artrie_{name}")
    function.argtypes = [
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.POINTER(ctypes.c_void_p),
    ]
    function.restype = ctypes.c_uint32
for name in ("create", "open"):
    function = getattr(_lib, f"ldict_persistent_vocab_{name}")
    function.argtypes = [
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.POINTER(ctypes.c_void_p),
    ]
    function.restype = ctypes.c_uint32
_lib.ldict_dictionary_checkpoint.argtypes = [ctypes.c_void_p]
_lib.ldict_dictionary_checkpoint.restype = ctypes.c_uint32
_lib.ldict_vocab_get_term.argtypes = [
    ctypes.c_void_p,
    ctypes.c_uint64,
    ctypes.c_void_p,
    ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_size_t),
    ctypes.POINTER(ctypes.c_uint8),
]
_lib.ldict_vocab_get_term.restype = ctypes.c_uint32

if _lib.ldict_abi_version() != 1:
    raise ImportError("libdictenstein native ABI version mismatch")


def abi_version() -> int:
    """Native ABI version (LDICT_ABI_VERSION); always 1 for this family."""
    return int(_lib.ldict_abi_version())


def api_revision() -> int:
    """Compatible-additions revision within the ABI version (LDICT_API_REVISION)."""
    return int(_lib.ldict_api_revision())


def _error() -> str:
    value = _lib.ldict_last_error_message()
    return value.decode("utf-8", "replace") if value else "native operation failed"


def _check(status: int) -> None:
    if status != 0:
        raise NativeError(status, _error())


def _text(key: str | bytes) -> bytes:
    return key.encode() if isinstance(key, str) else bytes(key)


DictionaryKey = str | bytes | tuple[int, ...]
DictionaryItem = tuple[DictionaryKey, int | None]


class DictionarySnapshot(Mapping[DictionaryKey, int | None]):
    """Immutable, insertion-ordered mapping copied from one native revision.

    Iteration order is unsigned unit-wise lexicographic order. ``keys()`` and
    ``items()`` are ordinary set-like Python views over this immutable mapping.
    """

    def __init__(self, entries: Iterable[DictionaryItem]) -> None:
        self._mapping = MappingProxyType(dict(entries))

    def __getitem__(self, key: DictionaryKey) -> int | None:
        return self._mapping[key]

    def __iter__(self) -> Iterator[DictionaryKey]:
        return iter(self._mapping)

    def __len__(self) -> int:
        return len(self._mapping)


class EntryStream(Iterator[DictionaryItem]):
    """Context-managed, bounded stream over one immutable native revision.

    Native arenas are decoded and released a batch at a time before Python
    code observes an entry. Closing early cancels the cursor and releases the
    retained revision deterministically.
    """

    _END = 1
    _LIMIT_EXCEEDED = 10
    _DEFAULT_ENTRIES = 256
    _DEFAULT_UNITS = 65_536

    def __init__(
        self,
        dictionary: _Dictionary,
        *,
        batch_size: int = _DEFAULT_ENTRIES,
        max_units: int | None = None,
    ) -> None:
        if batch_size <= 0:
            raise ValueError("batch_size must be positive")
        if max_units is not None and max_units <= 0:
            raise ValueError("max_units must be positive")
        if not dictionary._handle:
            raise RuntimeError("dictionary is closed")
        cursor = ctypes.c_void_p()
        info = _EntriesInfo()
        _check(
            _lib.ldict_dictionary_entries_open(
                dictionary._handle, ctypes.byref(cursor), ctypes.byref(info)
            )
        )
        self._cursor = cursor
        self._domain = UnitDomain(info.unit_domain)
        self._exact_len = int(info.exact_len) if info.flags & 1 else None
        self._identity = (
            (int(info.identity.producer), int(info.identity.revision))
            if info.flags & 2
            else None
        )
        self._pending: Iterator[DictionaryItem] = iter(())
        self._batch_size = batch_size
        self._max_units = max_units or max(self._DEFAULT_UNITS, batch_size)
        self._yielded = 0

    @property
    def exact_len(self) -> int | None:
        """Exact captured entry count when advertised by the provider."""
        return self._exact_len

    @property
    def snapshot_identity(self) -> tuple[int, int] | None:
        """Process-local producer/revision identity of the captured snapshot."""
        return self._identity

    def __iter__(self) -> EntryStream:
        return self

    def __length_hint__(self) -> int:
        if self._exact_len is None:
            return 0
        return max(0, self._exact_len - self._yielded)

    def _decode_key(self, units: int, offset: int, length: int) -> DictionaryKey:
        address = units
        if self._domain == UnitDomain.BYTE:
            return ctypes.string_at(address + offset, length)
        if self._domain == UnitDomain.UNICODE_SCALAR:
            data = ctypes.cast(address, ctypes.POINTER(ctypes.c_uint32))
            return "".join(chr(data[offset + index]) for index in range(length))
        data = ctypes.cast(address, ctypes.POINTER(ctypes.c_uint64))
        return tuple(int(data[offset + index]) for index in range(length))

    def _next_batch(self) -> list[DictionaryItem]:
        while True:
            limits = _EntryBatchLimits(
                self._batch_size,
                self._max_units,
                self._batch_size,
                0,
            )
            batch = _EntryBatch()
            status = int(
                _lib.ldict_entry_cursor_next(
                    self._cursor, ctypes.byref(limits), ctypes.byref(batch)
                )
            )
            if status == self._END:
                self.close()
                return []
            if status == self._LIMIT_EXCEEDED:
                maximum = ctypes.c_size_t(-1).value
                if self._max_units == maximum:
                    _check(status)
                self._max_units = min(maximum, self._max_units * 2)
                continue
            _check(status)

            decoded: list[DictionaryItem] = []
            try:
                units_address = int(batch.units or 0)
                for index in range(batch.entry_count):
                    descriptor = batch.entries[index]
                    key = self._decode_key(
                        units_address,
                        descriptor.unit_offset,
                        descriptor.unit_len,
                    )
                    if descriptor.value_len == 0:
                        value = None
                    elif descriptor.value_len == 1:
                        value = int(batch.values[descriptor.value_offset])
                    else:
                        raise RuntimeError(
                            "native entry has an invalid optional value width"
                        )
                    decoded.append((key, value))
            finally:
                _check(_lib.ldict_entry_cursor_release(self._cursor, batch.generation))
            return decoded

    def __next__(self) -> DictionaryItem:
        while self._cursor:
            try:
                item = next(self._pending)
                self._yielded += 1
                return item
            except StopIteration:
                batch = self._next_batch()
                if not batch:
                    break
                self._pending = iter(batch)
        raise StopIteration

    def close(self) -> None:
        """Cancel and close the cursor; idempotent."""
        if self._cursor:
            cursor = self._cursor
            _check(_lib.ldict_entry_cursor_cancel(cursor))
            _check(_lib.ldict_entry_cursor_free(cursor))
            self._cursor = ctypes.c_void_p()

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()

    def __del__(self) -> None:
        with suppress(Exception):
            self.close()


class _Dictionary(Mapping[DictionaryKey, int | None]):
    """Shared implementation for project-owned native dictionary handles.

    Queries started by another project retain the exact immutable revision
    visible at their start and remain valid after this facade is closed.
    """

    def __init__(self, domain: UnitDomain, handle: ctypes.c_void_p) -> None:
        self.domain = UnitDomain(domain)
        self._handle = handle

    @property
    def kind(self) -> int:
        """Stable native backend identifier."""
        value = ctypes.c_uint32()
        _check(_lib.ldict_dictionary_kind(self._handle, ctypes.byref(value)))
        return value.value

    @property
    def capabilities(self) -> int:
        """Bitset of operations implemented by this backend."""
        value = ctypes.c_uint64()
        _check(_lib.ldict_dictionary_capabilities(self._handle, ctypes.byref(value)))
        return value.value

    @property
    def native_resource(self) -> VtResource:
        """Borrow the shared resource for one synchronous retaining call."""
        if not self._handle:
            raise RuntimeError("dictionary is closed")
        resource = VtResource()
        _check(_lib.ldict_dictionary_resource(self._handle, ctypes.byref(resource)))
        return resource

    def __len__(self) -> int:
        length = ctypes.c_size_t()
        _check(_lib.ldict_dictionary_len(self._handle, ctypes.byref(length)))
        return length.value

    def __iter__(self) -> Iterator[DictionaryKey]:
        """Iterate keys from one immutable revision in lexicographic order."""
        return iter(self.snapshot())

    def __getitem__(self, key: DictionaryKey) -> int | None:
        found, value = self.lookup(key)
        if not found:
            raise KeyError(key)
        return value

    def insert(
        self, key: str | bytes | Sequence[int], value: int | None = None
    ) -> bool:
        changed = ctypes.c_uint8()
        if self.domain == UnitDomain.U64:
            values = (ctypes.c_uint64 * len(key))(*key)  # type: ignore[arg-type]
            _check(
                _lib.ldict_dictionary_insert_u64(
                    self._handle,
                    values,
                    len(key),  # type: ignore[arg-type]
                    _optional(value),
                    ctypes.byref(changed),
                )
            )
        else:
            data = _text(key)  # type: ignore[arg-type]
            _check(
                _lib.ldict_dictionary_insert_text(
                    self._handle,
                    data,
                    len(data),
                    _optional(value),
                    ctypes.byref(changed),
                )
            )
        return bool(changed.value)

    def remove(self, key: str | bytes | Sequence[int]) -> bool:
        changed = ctypes.c_uint8()
        if self.domain == UnitDomain.U64:
            values = (ctypes.c_uint64 * len(key))(*key)  # type: ignore[arg-type]
            _check(
                _lib.ldict_dictionary_remove_u64(
                    self._handle,
                    values,
                    len(key),
                    ctypes.byref(changed),  # type: ignore[arg-type]
                )
            )
        else:
            data = _text(key)  # type: ignore[arg-type]
            _check(
                _lib.ldict_dictionary_remove_text(
                    self._handle, data, len(data), ctypes.byref(changed)
                )
            )
        return bool(changed.value)

    def __contains__(self, key: object) -> bool:
        found = ctypes.c_uint8()
        if self.domain == UnitDomain.U64:
            if not isinstance(key, Sequence):
                return False
            values = (ctypes.c_uint64 * len(key))(*key)
            _check(
                _lib.ldict_dictionary_contains_u64(
                    self._handle, values, len(key), ctypes.byref(found)
                )
            )
        else:
            if not isinstance(key, (str, bytes)):
                return False
            data = _text(key)
            _check(
                _lib.ldict_dictionary_contains_text(
                    self._handle, data, len(data), ctypes.byref(found)
                )
            )
        return bool(found.value)

    def lookup(self, key: str | bytes | Sequence[int]) -> tuple[bool, int | None]:
        """Return ``(present, optional_value)`` without conflating absence and ``None``."""
        found = ctypes.c_uint8()
        value = _OptionalU64()
        if self.domain == UnitDomain.U64:
            values = (ctypes.c_uint64 * len(key))(*key)  # type: ignore[arg-type]
            _check(
                _lib.ldict_dictionary_get_u64(
                    self._handle,
                    values,
                    len(key),  # type: ignore[arg-type]
                    ctypes.byref(found),
                    ctypes.byref(value),
                )
            )
        else:
            data = _text(key)  # type: ignore[arg-type]
            _check(
                _lib.ldict_dictionary_get_text(
                    self._handle,
                    data,
                    len(data),
                    ctypes.byref(found),
                    ctypes.byref(value),
                )
            )
        return bool(found.value), value.value if value.has_value else None

    def stream_entries(
        self, *, batch_size: int = 256, max_units: int | None = None
    ) -> EntryStream:
        """Open a context-managed batched stream over one immutable revision.

        ``batch_size`` is a hard entry/value bound for each native lease.
        ``max_units`` may be supplied for workloads with known key sizes; the
        stream grows it only when one entry cannot fit.
        """
        return EntryStream(self, batch_size=batch_size, max_units=max_units)

    def snapshot(self) -> DictionarySnapshot:
        """Materialize one immutable mapping snapshot."""
        with self.stream_entries() as entries:
            return DictionarySnapshot(entries)

    def keys(self) -> KeysView[DictionaryKey]:
        """Return an immutable set-like keys view of one captured revision."""
        return self.snapshot().keys()

    def items(self) -> ItemsView[DictionaryKey, int | None]:
        """Return an immutable set-like items view of one captured revision."""
        return self.snapshot().items()

    def values(self) -> ValuesView[int | None]:
        """Return an immutable values view of one captured revision."""
        return self.snapshot().values()

    def update_many(
        self,
        entries: Iterable[tuple[str | bytes | Sequence[int], int | None]],
    ) -> int:
        materialized = list(entries)
        inserted = ctypes.c_size_t()
        if self.domain == UnitDomain.U64:
            buffers = [
                (ctypes.c_uint64 * len(key))(*key)  # type: ignore[arg-type]
                for key, _ in materialized
            ]
            descriptors = (_U64Entry * len(materialized))(
                *[
                    _U64Entry(
                        ctypes.cast(buffer, ctypes.c_void_p),
                        len(buffer),
                        _optional(value),
                    )
                    for buffer, (_, value) in zip(buffers, materialized, strict=True)
                ]
            )
            _check(
                _lib.ldict_dictionary_insert_u64_batch(
                    self._handle, descriptors, len(descriptors), ctypes.byref(inserted)
                )
            )
        else:
            buffers = [
                ctypes.create_string_buffer(_text(key)) for key, _ in materialized
            ]  # type: ignore[arg-type]
            descriptors = (_TextEntry * len(materialized))(
                *[
                    _TextEntry(
                        ctypes.cast(buffer, ctypes.c_void_p),
                        len(buffer.raw) - 1,
                        _optional(value),
                    )
                    for buffer, (_, value) in zip(buffers, materialized, strict=True)
                ]
            )
            _check(
                _lib.ldict_dictionary_insert_text_batch(
                    self._handle, descriptors, len(descriptors), ctypes.byref(inserted)
                )
            )
        return inserted.value

    def clear(self) -> None:
        _check(_lib.ldict_dictionary_clear(self._handle))

    def compact(self) -> int:
        reclaimed = ctypes.c_size_t()
        _check(_lib.ldict_dictionary_compact(self._handle, ctypes.byref(reclaimed)))
        return reclaimed.value

    def close(self) -> None:
        if self._handle:
            _lib.ldict_dictionary_free(self._handle)
            self._handle = ctypes.c_void_p()

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()

    def __del__(self) -> None:
        with suppress(Exception):
            self.close()


class _MutableDictionary(_Dictionary, MutableMapping[DictionaryKey, int | None]):
    """Standard mutable-mapping operations routed through native batch APIs."""

    def __setitem__(self, key: DictionaryKey, value: int | None) -> None:
        self.insert(key, value)

    def __delitem__(self, key: DictionaryKey) -> None:
        if not self.remove(key):
            raise KeyError(key)

    def update(
        self,
        other: Mapping[DictionaryKey, int | None] | Iterable[DictionaryItem] = (),
        /,
        **kwargs: int | None,
    ) -> None:
        if isinstance(other, Mapping):
            entries = list(other.items())
        else:
            entries = list(other)
        entries.extend(kwargs.items())
        self.update_many(entries)


class DynamicDawg(_MutableDictionary):
    """Mutable byte, Unicode-scalar, or u64 DynamicDAWG with full CRUD."""

    def __init__(self, domain: UnitDomain = UnitDomain.UNICODE_SCALAR) -> None:
        handle = ctypes.c_void_p()
        _check(_lib.ldict_dynamic_dawg_new(int(domain), ctypes.byref(handle)))
        super().__init__(domain, handle)


class DoubleArrayTrie(_Dictionary):
    """Immutable cache-local DoubleArrayTrie built in one native crossing."""

    def __init__(
        self,
        entries: Iterable[tuple[str | bytes, int | None] | str | bytes],
        domain: UnitDomain = UnitDomain.UNICODE_SCALAR,
    ) -> None:
        if domain == UnitDomain.U64:
            raise ValueError("DoubleArrayTrie supports byte and Unicode-scalar terms")
        materialized = [
            (entry, None) if isinstance(entry, (str, bytes)) else entry
            for entry in entries
        ]
        buffers = [
            ctypes.create_string_buffer(_text(term)) for term, _ in materialized
        ]
        descriptors = (_TextEntry * len(materialized))(
            *[
                _TextEntry(
                    ctypes.cast(buffer, ctypes.c_void_p),
                    len(buffer.raw) - 1,
                    _optional(value),
                )
                for buffer, (_, value) in zip(buffers, materialized, strict=True)
            ]
        )
        handle = ctypes.c_void_p()
        _check(
            _lib.ldict_double_array_trie_new(
                int(domain), descriptors, len(descriptors), ctypes.byref(handle)
            )
        )
        super().__init__(domain, handle)


class Scdawg(_MutableDictionary):
    """Mutable byte or Unicode SCDAWG with exact and substring operations."""

    def __init__(self, domain: UnitDomain = UnitDomain.UNICODE_SCALAR) -> None:
        if domain == UnitDomain.U64:
            raise ValueError("SCDAWG supports byte and Unicode-scalar terms")
        handle = ctypes.c_void_p()
        _check(_lib.ldict_scdawg_new(int(domain), ctypes.byref(handle)))
        super().__init__(domain, handle)

    def contains_substring(self, pattern: str | bytes) -> bool:
        data = _text(pattern)
        output = ctypes.c_uint8()
        _check(
            _lib.ldict_scdawg_contains_substring(
                self._handle, data, len(data), ctypes.byref(output)
            )
        )
        return bool(output.value)

    def frequency(self, pattern: str | bytes) -> int:
        data = _text(pattern)
        output = ctypes.c_size_t()
        _check(
            _lib.ldict_scdawg_substring_frequency(
                self._handle, data, len(data), ctypes.byref(output)
            )
        )
        return output.value


class PersistentARTrie(_MutableDictionary):
    """Filesystem-backed byte, Unicode, or native-u64 adaptive radix trie."""

    def __init__(self, *_args: object, **_kwargs: object) -> None:
        raise TypeError("use PersistentARTrie.create() or PersistentARTrie.open()")

    @classmethod
    def _load(
        cls, path: str | os.PathLike[str], domain: UnitDomain, create: bool
    ) -> PersistentARTrie:
        encoded = os.fsencode(path)
        handle = ctypes.c_void_p()
        function = (
            _lib.ldict_persistent_artrie_create
            if create
            else _lib.ldict_persistent_artrie_open
        )
        _check(function(int(domain), encoded, len(encoded), ctypes.byref(handle)))
        instance = object.__new__(cls)
        _Dictionary.__init__(instance, domain, handle)
        return instance

    @classmethod
    def create(
        cls,
        path: str | os.PathLike[str],
        domain: UnitDomain = UnitDomain.UNICODE_SCALAR,
    ) -> PersistentARTrie:
        return cls._load(path, domain, True)

    @classmethod
    def open(
        cls,
        path: str | os.PathLike[str],
        domain: UnitDomain = UnitDomain.UNICODE_SCALAR,
    ) -> PersistentARTrie:
        return cls._load(path, domain, False)

    def checkpoint(self) -> None:
        _check(_lib.ldict_dictionary_checkpoint(self._handle))


class PersistentVocabulary(_MutableDictionary):
    """Filesystem-backed bidirectional Unicode term/u64-index vocabulary."""

    def __init__(self, *_args: object, **_kwargs: object) -> None:
        raise TypeError(
            "use PersistentVocabulary.create() or PersistentVocabulary.open()"
        )

    @classmethod
    def _load(cls, path: str | os.PathLike[str], create: bool) -> PersistentVocabulary:
        encoded = os.fsencode(path)
        handle = ctypes.c_void_p()
        function = (
            _lib.ldict_persistent_vocab_create
            if create
            else _lib.ldict_persistent_vocab_open
        )
        _check(function(encoded, len(encoded), ctypes.byref(handle)))
        instance = object.__new__(cls)
        _Dictionary.__init__(instance, UnitDomain.UNICODE_SCALAR, handle)
        return instance

    @classmethod
    def create(cls, path: str | os.PathLike[str]) -> PersistentVocabulary:
        return cls._load(path, True)

    @classmethod
    def open(cls, path: str | os.PathLike[str]) -> PersistentVocabulary:
        return cls._load(path, False)

    def checkpoint(self) -> None:
        _check(_lib.ldict_dictionary_checkpoint(self._handle))

    def term(self, index: int) -> str | None:
        if not 0 <= index < 2**64:
            raise ValueError("vocabulary index is outside u64")
        length = ctypes.c_size_t()
        found = ctypes.c_uint8()
        _check(
            _lib.ldict_vocab_get_term(
                self._handle,
                index,
                None,
                0,
                ctypes.byref(length),
                ctypes.byref(found),
            )
        )
        if not found.value:
            return None
        output = (ctypes.c_uint8 * length.value)()
        _check(
            _lib.ldict_vocab_get_term(
                self._handle,
                index,
                output,
                length.value,
                ctypes.byref(length),
                ctypes.byref(found),
            )
        )
        return bytes(output).decode()

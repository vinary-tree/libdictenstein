"""ctypes facade over libdictenstein's project-owned C ABI."""

from __future__ import annotations

import ctypes
import ctypes.util
import os
import platform
from collections.abc import Iterable, Sequence
from pathlib import Path

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
_lib.ldict_last_error_message.restype = ctypes.c_char_p
_lib.ldict_dynamic_dawg_new.argtypes = [ctypes.c_uint32, ctypes.POINTER(ctypes.c_void_p)]
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
_lib.ldict_dictionary_compact.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t)]
_lib.ldict_dictionary_compact.restype = ctypes.c_uint32

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


def _error() -> str:
    value = _lib.ldict_last_error_message()
    return value.decode("utf-8", "replace") if value else "native operation failed"


def _check(status: int) -> None:
    if status != 0:
        raise NativeError(status, _error())


def _text(key: str | bytes) -> bytes:
    return key.encode() if isinstance(key, str) else bytes(key)


class _Dictionary:
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
                    self._handle, values, len(key), ctypes.byref(changed)  # type: ignore[arg-type]
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

    def get(self, key: str | bytes | Sequence[int]) -> tuple[bool, int | None]:
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
                    _U64Entry(ctypes.cast(buffer, ctypes.c_void_p), len(buffer), _optional(value))
                    for buffer, (_, value) in zip(buffers, materialized, strict=True)
                ]
            )
            _check(
                _lib.ldict_dictionary_insert_u64_batch(
                    self._handle, descriptors, len(descriptors), ctypes.byref(inserted)
                )
            )
        else:
            buffers = [ctypes.create_string_buffer(_text(key)) for key, _ in materialized]  # type: ignore[arg-type]
            descriptors = (_TextEntry * len(materialized))(
                *[
                    _TextEntry(ctypes.cast(buffer, ctypes.c_void_p), len(buffer.raw) - 1, _optional(value))
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

    def __enter__(self) -> _Dictionary:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()

    def __del__(self) -> None:
        try:
            self.close()
        except Exception:
            pass


class DynamicDawg(_Dictionary):
    """Mutable byte, Unicode-scalar, or u64 DynamicDAWG with full CRUD."""

    def __init__(self, domain: UnitDomain = UnitDomain.UNICODE_SCALAR) -> None:
        handle = ctypes.c_void_p()
        _check(_lib.ldict_dynamic_dawg_new(int(domain), ctypes.byref(handle)))
        super().__init__(domain, handle)


class DoubleArrayTrie(_Dictionary):
    """Immutable cache-local DoubleArrayTrie built in one native crossing."""

    def __init__(
        self,
        entries: Iterable[tuple[str, int | None] | str],
        domain: UnitDomain = UnitDomain.UNICODE_SCALAR,
    ) -> None:
        if domain == UnitDomain.U64:
            raise ValueError("DoubleArrayTrie supports byte and Unicode-scalar terms")
        materialized = [
            (entry, None) if isinstance(entry, str) else entry for entry in entries
        ]
        buffers = [ctypes.create_string_buffer(term.encode()) for term, _ in materialized]
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


class Scdawg(_Dictionary):
    """Mutable byte or Unicode SCDAWG with exact and substring operations."""

    def __init__(self, domain: UnitDomain = UnitDomain.UNICODE_SCALAR) -> None:
        if domain == UnitDomain.U64:
            raise ValueError("SCDAWG supports byte and Unicode-scalar terms")
        handle = ctypes.c_void_p()
        _check(_lib.ldict_scdawg_new(int(domain), ctypes.byref(handle)))
        super().__init__(domain, handle)

    def contains_substring(self, pattern: str) -> bool:
        data = pattern.encode()
        output = ctypes.c_uint8()
        _check(
            _lib.ldict_scdawg_contains_substring(
                self._handle, data, len(data), ctypes.byref(output)
            )
        )
        return bool(output.value)

    def frequency(self, pattern: str) -> int:
        data = pattern.encode()
        output = ctypes.c_size_t()
        _check(
            _lib.ldict_scdawg_substring_frequency(
                self._handle, data, len(data), ctypes.byref(output)
            )
        )
        return output.value


class PersistentARTrie(_Dictionary):
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


class PersistentVocabulary(_Dictionary):
    """Filesystem-backed bidirectional Unicode term/u64-index vocabulary."""

    def __init__(self, *_args: object, **_kwargs: object) -> None:
        raise TypeError(
            "use PersistentVocabulary.create() or PersistentVocabulary.open()"
        )

    @classmethod
    def _load(
        cls, path: str | os.PathLike[str], create: bool
    ) -> PersistentVocabulary:
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

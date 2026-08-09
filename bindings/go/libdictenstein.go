// Package libdictenstein exposes Vinary Tree dictionary backends and lends
// their retained resources to independent consumers without serialization.
package libdictenstein

/*
#cgo CFLAGS: -std=c17 -I${SRCDIR}/../../include -I${SRCDIR}/../../../liblevenshtein-rust/vinary-tree-interop/include
#cgo LDFLAGS: -llibdictenstein
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "libdictenstein.h"

static LdictStatus go_ldict_resource_words(const LdictDictionary* dictionary,
                                           uintptr_t* context,
                                           uintptr_t* vtable) {
    VtResource resource = {0};
    LdictStatus status = ldict_dictionary_resource(dictionary, &resource);
    if (status == LDICT_STATUS_OK) {
        *context = (uintptr_t)resource.context;
        *vtable = (uintptr_t)resource.vtable;
    }
    return status;
}
*/
import "C"

import (
	"errors"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// UnitDomain selects dictionary transition units.
type UnitDomain uint32

const (
	ByteDomain UnitDomain = iota + 1
	UnicodeScalarDomain
	U64Domain
)

// BackendKind identifies a concrete dictionary implementation.
type BackendKind uint32

const (
	DynamicDawgKind BackendKind = iota + 1
	DoubleArrayTrieKind
	ScdawgKind
	PersistentArtrieKind
	PersistentVocabularyKind
)

// Capability bits returned by Dictionary.Capabilities.
const (
	CanRead uint64 = 1 << iota
	CanInsert
	CanRemove
	CanClear
	CanCompact
	CanSubstring
	CanCheckpoint
)

// Lookup distinguishes absence from a present term without a mapped value.
type Lookup struct {
	Found bool
	Value *uint64
}

// Error is a stable native dictionary status.
type Error struct {
	Status  uint32
	Message string
}

func (e *Error) Error() string {
	return fmt.Sprintf("libdictenstein status %d: %s", e.Status, e.Message)
}
func check(status C.LdictStatus) error {
	if status == C.LDICT_STATUS_OK {
		return nil
	}
	return &Error{Status: uint32(status), Message: C.GoString(C.ldict_last_error_message())}
}

func optional(value *uint64) C.LdictOptionalU64 {
	var result C.LdictOptionalU64
	if value != nil {
		result.value = C.uint64_t(*value)
		result.has_value = 1
	}
	return result
}
func valueOf(value uint64) *uint64 { result := value; return &result }

// AbiVersion reports the native ABI version (LDICT_ABI_VERSION); always 1 for
// this family. Consumers gate on it before trusting any other symbol.
func AbiVersion() uint32 { return uint32(C.ldict_abi_version()) }

// ApiRevision reports the compatible-additions revision within the ABI version
// (LDICT_API_REVISION). It increases monotonically as symbols are added.
func ApiRevision() uint32 { return uint32(C.ldict_api_revision()) }

// Entry is one term/value mutation descriptor.
type Entry struct {
	Term  string
	Value *uint64
}

// Dictionary provides common exact lookup and retained-resource operations.
type Dictionary struct {
	mu      sync.RWMutex
	pointer *C.LdictDictionary
}

func wrap(pointer *C.LdictDictionary) *Dictionary {
	result := &Dictionary{pointer: pointer}
	runtime.SetFinalizer(result, (*Dictionary).finalize)
	return result
}
func (d *Dictionary) finalize() { _ = d.Close() }

// WithResource implements the shared allocation-free resource interface.
func (d *Dictionary) WithResource(callback func(context, vtable uintptr) error) error {
	if callback == nil {
		return errors.New("resource callback is nil")
	}
	d.mu.RLock()
	defer d.mu.RUnlock()
	if d.pointer == nil {
		return errors.New("dictionary is closed")
	}
	var context, vtable C.uintptr_t
	if err := check(C.go_ldict_resource_words(d.pointer, &context, &vtable)); err != nil {
		return err
	}
	return callback(uintptr(context), uintptr(vtable))
}

// Close releases the producer handle; retained transducers remain valid.
func (d *Dictionary) Close() error {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.pointer != nil {
		C.ldict_dictionary_free(d.pointer)
		d.pointer = nil
		runtime.SetFinalizer(d, nil)
	}
	return nil
}
func (d *Dictionary) read(call func(*C.LdictDictionary) error) error {
	d.mu.RLock()
	defer d.mu.RUnlock()
	if d.pointer == nil {
		return errors.New("dictionary is closed")
	}
	return call(d.pointer)
}

// Kind returns the concrete backend.
func (d *Dictionary) Kind() (BackendKind, error) {
	var result C.uint32_t
	err := d.read(func(pointer *C.LdictDictionary) error { return check(C.ldict_dictionary_kind(pointer, &result)) })
	return BackendKind(result), err
}

// Capabilities returns the stable runtime capability bits.
func (d *Dictionary) Capabilities() (uint64, error) {
	var result C.uint64_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		return check(C.ldict_dictionary_capabilities(pointer, &result))
	})
	return uint64(result), err
}

// Len returns the number of complete terms.
func (d *Dictionary) Len() (uint, error) {
	var result C.size_t
	err := d.read(func(pointer *C.LdictDictionary) error { return check(C.ldict_dictionary_len(pointer, &result)) })
	return uint(result), err
}

func textPointer(text []byte) *C.uint8_t {
	if len(text) == 0 {
		return nil
	}
	return (*C.uint8_t)(unsafe.Pointer(&text[0]))
}

// Contains tests a text or raw-byte term.
func (d *Dictionary) Contains(term string) (bool, error) {
	text := []byte(term)
	var result C.uint8_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		status := C.ldict_dictionary_contains_text(pointer, textPointer(text), C.size_t(len(text)), &result)
		runtime.KeepAlive(text)
		return check(status)
	})
	return result != 0, err
}

// Get performs a three-state text lookup.
func (d *Dictionary) Get(term string) (Lookup, error) {
	text := []byte(term)
	var found C.uint8_t
	var value C.LdictOptionalU64
	err := d.read(func(pointer *C.LdictDictionary) error {
		status := C.ldict_dictionary_get_text(pointer, textPointer(text), C.size_t(len(text)), &found, &value)
		runtime.KeepAlive(text)
		return check(status)
	})
	result := Lookup{Found: found != 0}
	if value.has_value != 0 {
		result.Value = valueOf(uint64(value.value))
	}
	return result, err
}

// ContainsU64 tests a token term.
func (d *Dictionary) ContainsU64(term []uint64) (bool, error) {
	var data *C.uint64_t
	if len(term) != 0 {
		data = (*C.uint64_t)(unsafe.Pointer(&term[0]))
	}
	var result C.uint8_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		status := C.ldict_dictionary_contains_u64(pointer, data, C.size_t(len(term)), &result)
		runtime.KeepAlive(term)
		return check(status)
	})
	return result != 0, err
}

// GetU64 performs a three-state token lookup.
func (d *Dictionary) GetU64(term []uint64) (Lookup, error) {
	var data *C.uint64_t
	if len(term) != 0 {
		data = (*C.uint64_t)(unsafe.Pointer(&term[0]))
	}
	var found C.uint8_t
	var value C.LdictOptionalU64
	err := d.read(func(pointer *C.LdictDictionary) error {
		status := C.ldict_dictionary_get_u64(pointer, data, C.size_t(len(term)), &found, &value)
		runtime.KeepAlive(term)
		return check(status)
	})
	result := Lookup{Found: found != 0}
	if value.has_value != 0 {
		result.Value = valueOf(uint64(value.value))
	}
	return result, err
}

func (d *Dictionary) put(term string, value *uint64) (bool, error) {
	text := []byte(term)
	var inserted C.uint8_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		status := C.ldict_dictionary_insert_text(pointer, textPointer(text), C.size_t(len(text)), optional(value), &inserted)
		runtime.KeepAlive(text)
		return check(status)
	})
	return inserted != 0, err
}
func (d *Dictionary) remove(term string) (bool, error) {
	text := []byte(term)
	var removed C.uint8_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		status := C.ldict_dictionary_remove_text(pointer, textPointer(text), C.size_t(len(text)), &removed)
		runtime.KeepAlive(text)
		return check(status)
	})
	return removed != 0, err
}
func (d *Dictionary) putU64(term []uint64, value *uint64) (bool, error) {
	var data *C.uint64_t
	if len(term) != 0 {
		data = (*C.uint64_t)(unsafe.Pointer(&term[0]))
	}
	var inserted C.uint8_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		status := C.ldict_dictionary_insert_u64(pointer, data, C.size_t(len(term)), optional(value), &inserted)
		runtime.KeepAlive(term)
		return check(status)
	})
	return inserted != 0, err
}
func (d *Dictionary) removeU64(term []uint64) (bool, error) {
	var data *C.uint64_t
	if len(term) != 0 {
		data = (*C.uint64_t)(unsafe.Pointer(&term[0]))
	}
	var removed C.uint8_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		status := C.ldict_dictionary_remove_u64(pointer, data, C.size_t(len(term)), &removed)
		runtime.KeepAlive(term)
		return check(status)
	})
	return removed != 0, err
}

// DynamicDawg provides full mutable CRUD.
type DynamicDawg struct{ *Dictionary }

// NewDynamicDawg creates an empty dictionary.
func NewDynamicDawg(domain UnitDomain) (*DynamicDawg, error) {
	var pointer *C.LdictDictionary
	if err := check(C.ldict_dynamic_dawg_new(C.uint32_t(domain), &pointer)); err != nil {
		return nil, err
	}
	return &DynamicDawg{wrap(pointer)}, nil
}
func (d *DynamicDawg) Put(term string, value *uint64) (bool, error) { return d.put(term, value) }
func (d *DynamicDawg) Remove(term string) (bool, error)             { return d.remove(term) }
func (d *DynamicDawg) PutU64(term []uint64, value *uint64) (bool, error) {
	return d.putU64(term, value)
}
func (d *DynamicDawg) RemoveU64(term []uint64) (bool, error) { return d.removeU64(term) }
func (d *DynamicDawg) Clear() error {
	return d.read(func(pointer *C.LdictDictionary) error { return check(C.ldict_dictionary_clear(pointer)) })
}
func (d *DynamicDawg) Compact() (uint, error) {
	var result C.size_t
	err := d.read(func(pointer *C.LdictDictionary) error { return check(C.ldict_dictionary_compact(pointer, &result)) })
	return uint(result), err
}

func withTextEntries(entries []Entry, callback func(*C.LdictTextEntry, C.size_t) error) error {
	var bytes uint
	for _, entry := range entries {
		bytes += uint(len(entry.Term))
	}
	descriptorBytes := C.size_t(len(entries)) * C.size_t(C.sizeof_LdictTextEntry)
	descriptors := C.calloc(1, descriptorBytes)
	if descriptors == nil && len(entries) != 0 {
		return errors.New("descriptor allocation failed")
	}
	defer C.free(descriptors)
	arena := C.malloc(C.size_t(max(bytes, 1)))
	if arena == nil {
		return errors.New("term arena allocation failed")
	}
	defer C.free(arena)
	slice := unsafe.Slice((*C.LdictTextEntry)(descriptors), len(entries))
	offset := uintptr(0)
	for index, entry := range entries {
		data := []byte(entry.Term)
		destination := unsafe.Add(arena, offset)
		if len(data) != 0 {
			C.memcpy(destination, unsafe.Pointer(&data[0]), C.size_t(len(data)))
		}
		slice[index].data = (*C.uint8_t)(destination)
		slice[index].len = C.size_t(len(data))
		slice[index].value = optional(entry.Value)
		offset += uintptr(len(data))
	}
	return callback((*C.LdictTextEntry)(descriptors), C.size_t(len(entries)))
}

// PutAll inserts one contiguous mutation batch.
func (d *DynamicDawg) PutAll(entries []Entry) (uint, error) {
	var result C.size_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		return withTextEntries(entries, func(items *C.LdictTextEntry, count C.size_t) error {
			return check(C.ldict_dictionary_insert_text_batch(pointer, items, count, &result))
		})
	})
	return uint(result), err
}

// DoubleArrayTrie is an immutable compact trie.
type DoubleArrayTrie struct{ *Dictionary }

// NewDoubleArrayTrie builds a trie from one contiguous descriptor batch.
func NewDoubleArrayTrie(entries []Entry, domain UnitDomain) (*DoubleArrayTrie, error) {
	var pointer *C.LdictDictionary
	err := withTextEntries(entries, func(items *C.LdictTextEntry, count C.size_t) error {
		return check(C.ldict_double_array_trie_new(C.uint32_t(domain), items, count, &pointer))
	})
	if err != nil {
		return nil, err
	}
	return &DoubleArrayTrie{wrap(pointer)}, nil
}

// Scdawg is an incremental suffix dictionary.
type Scdawg struct{ *Dictionary }

func NewScdawg(domain UnitDomain) (*Scdawg, error) {
	var pointer *C.LdictDictionary
	if err := check(C.ldict_scdawg_new(C.uint32_t(domain), &pointer)); err != nil {
		return nil, err
	}
	return &Scdawg{wrap(pointer)}, nil
}
func (d *Scdawg) Put(term string, value *uint64) (bool, error) { return d.put(term, value) }
func (d *Scdawg) ContainsSubstring(term string) (bool, error) {
	text := []byte(term)
	var result C.uint8_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		status := C.ldict_scdawg_contains_substring(pointer, textPointer(text), C.size_t(len(text)), &result)
		runtime.KeepAlive(text)
		return check(status)
	})
	return result != 0, err
}
func (d *Scdawg) SubstringFrequency(term string) (uint, error) {
	text := []byte(term)
	var result C.size_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		status := C.ldict_scdawg_substring_frequency(pointer, textPointer(text), C.size_t(len(text)), &result)
		runtime.KeepAlive(text)
		return check(status)
	})
	return uint(result), err
}

// PersistentArtrie is a filesystem-backed adaptive radix trie.
type PersistentArtrie struct{ *Dictionary }

func persistent(path string, domain UnitDomain, create bool) (*PersistentArtrie, error) {
	text := []byte(path)
	var pointer *C.LdictDictionary
	var status C.LdictStatus
	if create {
		status = C.ldict_persistent_artrie_create(C.uint32_t(domain), textPointer(text), C.size_t(len(text)), &pointer)
	} else {
		status = C.ldict_persistent_artrie_open(C.uint32_t(domain), textPointer(text), C.size_t(len(text)), &pointer)
	}
	runtime.KeepAlive(text)
	if err := check(status); err != nil {
		return nil, err
	}
	return &PersistentArtrie{wrap(pointer)}, nil
}
func CreatePersistentArtrie(path string, domain UnitDomain) (*PersistentArtrie, error) {
	return persistent(path, domain, true)
}
func OpenPersistentArtrie(path string, domain UnitDomain) (*PersistentArtrie, error) {
	return persistent(path, domain, false)
}
func (d *PersistentArtrie) Put(term string, value *uint64) (bool, error) { return d.put(term, value) }
func (d *PersistentArtrie) Remove(term string) (bool, error)             { return d.remove(term) }
func (d *PersistentArtrie) PutU64(term []uint64, value *uint64) (bool, error) {
	return d.putU64(term, value)
}
func (d *PersistentArtrie) RemoveU64(term []uint64) (bool, error) { return d.removeU64(term) }
func (d *PersistentArtrie) Checkpoint() error {
	return d.read(func(pointer *C.LdictDictionary) error { return check(C.ldict_dictionary_checkpoint(pointer)) })
}

// PersistentVocabulary is a bidirectional durable term/index map.
type PersistentVocabulary struct{ *Dictionary }

func vocabulary(path string, create bool) (*PersistentVocabulary, error) {
	text := []byte(path)
	var pointer *C.LdictDictionary
	var status C.LdictStatus
	if create {
		status = C.ldict_persistent_vocab_create(textPointer(text), C.size_t(len(text)), &pointer)
	} else {
		status = C.ldict_persistent_vocab_open(textPointer(text), C.size_t(len(text)), &pointer)
	}
	runtime.KeepAlive(text)
	if err := check(status); err != nil {
		return nil, err
	}
	return &PersistentVocabulary{wrap(pointer)}, nil
}
func CreatePersistentVocabulary(path string) (*PersistentVocabulary, error) {
	return vocabulary(path, true)
}
func OpenPersistentVocabulary(path string) (*PersistentVocabulary, error) {
	return vocabulary(path, false)
}
func (d *PersistentVocabulary) Put(term string, index uint64) (bool, error) {
	return d.put(term, &index)
}
func (d *PersistentVocabulary) Term(index uint64) (string, bool, error) {
	var length C.size_t
	var found C.uint8_t
	err := d.read(func(pointer *C.LdictDictionary) error {
		return check(C.ldict_vocab_get_term(pointer, C.uint64_t(index), nil, 0, &length, &found))
	})
	if err != nil || found == 0 {
		return "", false, err
	}
	output := make([]byte, int(length))
	err = d.read(func(pointer *C.LdictDictionary) error {
		return check(C.ldict_vocab_get_term(pointer, C.uint64_t(index), textPointer(output), C.size_t(len(output)), &length, &found))
	})
	return string(output), found != 0, err
}
func (d *PersistentVocabulary) Checkpoint() error {
	return d.read(func(pointer *C.LdictDictionary) error { return check(C.ldict_dictionary_checkpoint(pointer)) })
}

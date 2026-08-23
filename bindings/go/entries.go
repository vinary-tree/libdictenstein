package libdictenstein

/*
#cgo CFLAGS: -std=c17 -I${SRCDIR}/../../include -I${SRCDIR}/../../../vinary-tree-interop/include
#cgo LDFLAGS: -llibdictenstein
#include "libdictenstein.h"
*/
import "C"

import (
	"errors"
	"fmt"
	"iter"
	"runtime"
	"sync"
	"unsafe"
)

// EntryBatchLimits are hard bounds for one native leased batch.
type EntryBatchLimits struct {
	MaxEntries uint
	MaxUnits   uint
	MaxValues  uint
}

// DefaultEntryBatchLimits balances crossings with bounded temporary storage.
var DefaultEntryBatchLimits = EntryBatchLimits{MaxEntries: 256, MaxUnits: 4096, MaxValues: 256}

// EntriesInfo describes the immutable revision captured by an entry cursor.
type EntriesInfo struct {
	UnitDomain          UnitDomain
	ExactLen            uint
	HasExactLen         bool
	ProducerIdentity    uint64
	RevisionIdentity    uint64
	HasSnapshotIdentity bool
}

// SnapshotEntry is a host-owned dictionary member. Exactly one key field is
// selected by Domain. A nil Value means a present term without a mapped value;
// a non-nil pointer can contain every uint64 value, including zero and max.
type SnapshotEntry struct {
	Domain UnitDomain
	Bytes  []byte
	Text   string
	U64    []uint64
	Value  *uint64
}

// EntrySnapshot is a materialized immutable revision and is safe to retain
// after its source dictionary and native cursor are closed.
type EntrySnapshot struct {
	Info    EntriesInfo
	Entries []SnapshotEntry
}

// EntryStream is a single-pass bounded snapshot cursor. Next copies every key
// before returning it. Close or Cancel is required when driving Next directly;
// Seq and Seq2 close automatically on exhaustion, early break, or panic.
type EntryStream struct {
	mu     sync.Mutex
	cursor *C.LdictEntryCursor
	info   EntriesInfo
	limits C.LdictEntryBatchLimits
	batch  C.LdictEntryBatch
	index  int
	leased bool
	ended  bool
	err    error
}

func nativeEntriesInfo(info C.LdictEntriesInfo) EntriesInfo {
	flags := uint64(info.flags)
	return EntriesInfo{
		UnitDomain:          UnitDomain(info.unit_domain),
		ExactLen:            uint(info.exact_len),
		HasExactLen:         flags&uint64(C.LDICT_ENTRIES_INFO_FLAG_EXACT_LEN) != 0,
		ProducerIdentity:    uint64(info.identity.producer),
		RevisionIdentity:    uint64(info.identity.revision),
		HasSnapshotIdentity: flags&uint64(C.LDICT_ENTRIES_INFO_FLAG_SNAPSHOT_IDENTITY) != 0,
	}
}

func validateEntryBatchLimits(limits EntryBatchLimits) error {
	if limits.MaxEntries == 0 {
		return errors.New("entry batch MaxEntries must be nonzero")
	}
	maxInt := uint(^uint(0) >> 1)
	if limits.MaxEntries > maxInt || limits.MaxUnits > maxInt || limits.MaxValues > maxInt {
		return errors.New("entry batch limit exceeds the host index range")
	}
	return nil
}

// OpenEntryStream captures the dictionary's current immutable revision.
func (d *Dictionary) OpenEntryStream(limits EntryBatchLimits) (*EntryStream, error) {
	if err := validateEntryBatchLimits(limits); err != nil {
		return nil, err
	}
	stream := &EntryStream{
		limits: C.LdictEntryBatchLimits{
			max_entries: C.size_t(limits.MaxEntries),
			max_units:   C.size_t(limits.MaxUnits),
			max_values:  C.size_t(limits.MaxValues),
		},
	}
	var nativeInfo C.LdictEntriesInfo
	err := d.read(func(pointer *C.LdictDictionary) error {
		return check(C.ldict_dictionary_entries_open(pointer, &stream.cursor, &nativeInfo))
	})
	if err != nil {
		return nil, err
	}
	stream.info = nativeEntriesInfo(nativeInfo)
	runtime.SetFinalizer(stream, (*EntryStream).finalize)
	return stream, nil
}

// Info returns metadata captured atomically with this stream.
func (s *EntryStream) Info() EntriesInfo {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.info
}

func checkedRange(offset, length, total C.size_t, name string) (int, int, error) {
	maxInt := uint64(^uint(0) >> 1)
	offset64, length64, total64 := uint64(offset), uint64(length), uint64(total)
	if total64 > maxInt || offset64 > total64 || length64 > total64-offset64 {
		return 0, 0, fmt.Errorf("invalid native %s range", name)
	}
	return int(offset64), int(length64), nil
}

func (s *EntryStream) copyEntry(index int) (SnapshotEntry, error) {
	count := int(s.batch.entry_count)
	if index < 0 || index >= count || s.batch.entries == nil {
		return SnapshotEntry{}, errors.New("invalid native entry descriptor index")
	}
	descriptors := unsafe.Slice((*C.LdictEntry)(unsafe.Pointer(s.batch.entries)), count)
	descriptor := descriptors[index]
	offset, length, err := checkedRange(
		descriptor.unit_offset, descriptor.unit_len, s.batch.unit_count, "unit",
	)
	if err != nil {
		return SnapshotEntry{}, err
	}
	result := SnapshotEntry{Domain: s.info.UnitDomain}
	switch s.info.UnitDomain {
	case ByteDomain:
		result.Bytes = make([]byte, length)
		if length != 0 {
			if s.batch.units == nil {
				return SnapshotEntry{}, errors.New("native byte arena is null")
			}
			arena := unsafe.Slice((*byte)(s.batch.units), int(s.batch.unit_count))
			copy(result.Bytes, arena[offset:offset+length])
		}
	case UnicodeScalarDomain:
		if length != 0 && s.batch.units == nil {
			return SnapshotEntry{}, errors.New("native Unicode-scalar arena is null")
		}
		arena := unsafe.Slice((*C.uint32_t)(s.batch.units), int(s.batch.unit_count))
		runes := make([]rune, length)
		for i, scalar := range arena[offset : offset+length] {
			runes[i] = rune(scalar)
		}
		result.Text = string(runes)
	case U64Domain:
		result.U64 = make([]uint64, length)
		if length != 0 {
			if s.batch.units == nil {
				return SnapshotEntry{}, errors.New("native u64 arena is null")
			}
			arena := unsafe.Slice((*C.uint64_t)(s.batch.units), int(s.batch.unit_count))
			for i, unit := range arena[offset : offset+length] {
				result.U64[i] = uint64(unit)
			}
		}
	default:
		return SnapshotEntry{}, fmt.Errorf("unknown native unit domain %d", s.info.UnitDomain)
	}

	switch descriptor.value_len {
	case 0:
	case 1:
		valueOffset, _, rangeErr := checkedRange(
			descriptor.value_offset, 1, s.batch.value_count, "value",
		)
		if rangeErr != nil {
			return SnapshotEntry{}, rangeErr
		}
		if s.batch.values == nil {
			return SnapshotEntry{}, errors.New("native value arena is null")
		}
		values := unsafe.Slice((*C.uint64_t)(unsafe.Pointer(s.batch.values)), int(s.batch.value_count))
		value := uint64(values[valueOffset])
		result.Value = &value
	default:
		return SnapshotEntry{}, errors.New("invalid native optional-u64 descriptor")
	}
	return result, nil
}

func (s *EntryStream) releaseLocked() error {
	if !s.leased {
		return nil
	}
	if err := check(C.ldict_entry_cursor_release(s.cursor, s.batch.generation)); err != nil {
		return err
	}
	s.leased = false
	s.batch = C.LdictEntryBatch{}
	s.index = 0
	return nil
}

func (s *EntryStream) closeLocked(cancel bool) error {
	if s.cursor == nil {
		return nil
	}
	var result error
	if cancel {
		result = check(C.ldict_entry_cursor_cancel(s.cursor))
	}
	result = errors.Join(result, s.releaseLocked())
	if err := check(C.ldict_entry_cursor_free(s.cursor)); err != nil {
		result = errors.Join(result, err)
		return result
	}
	s.cursor = nil
	s.ended = true
	runtime.SetFinalizer(s, nil)
	return result
}

func (s *EntryStream) failLocked(err error) error {
	s.err = err
	s.err = errors.Join(s.err, s.closeLocked(true))
	return s.err
}

// Next returns one copied entry. ok is false after sticky exhaustion.
func (s *EntryStream) Next() (entry SnapshotEntry, ok bool, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.err != nil {
		return SnapshotEntry{}, false, s.err
	}
	if s.ended || s.cursor == nil {
		return SnapshotEntry{}, false, nil
	}
	if !s.leased {
		status := C.ldict_entry_cursor_next(s.cursor, &s.limits, &s.batch)
		if status == C.LDICT_STATUS_END {
			s.ended = true
			if closeErr := s.closeLocked(false); closeErr != nil {
				return SnapshotEntry{}, false, s.failLocked(closeErr)
			}
			return SnapshotEntry{}, false, nil
		}
		if nextErr := check(status); nextErr != nil {
			return SnapshotEntry{}, false, s.failLocked(nextErr)
		}
		s.leased = true
		s.index = 0
	}
	entry, copyErr := s.copyEntry(s.index)
	if copyErr != nil {
		return SnapshotEntry{}, false, s.failLocked(copyErr)
	}
	s.index++
	if s.index == int(s.batch.entry_count) {
		if releaseErr := s.releaseLocked(); releaseErr != nil {
			return SnapshotEntry{}, false, s.failLocked(releaseErr)
		}
	}
	return entry, true, nil
}

// Err reports the first error observed by Seq or direct traversal.
func (s *EntryStream) Err() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.err
}

// Close deterministically cancels traversal, releases a live lease, and frees
// the cursor. It is idempotent.
func (s *EntryStream) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	err := s.closeLocked(true)
	if s.err == nil {
		s.err = err
	}
	return err
}

// Cancel is the explicit early-exit spelling; it also closes the cursor.
func (s *EntryStream) Cancel() error { return s.Close() }

func (s *EntryStream) finalize() { _ = s.Close() }

// Seq is Go 1.23 range-compatible. Errors stop iteration and are available
// through Err. The stream is always closed when the range exits.
func (s *EntryStream) Seq() iter.Seq[SnapshotEntry] {
	return func(yield func(SnapshotEntry) bool) {
		defer s.Close()
		for {
			entry, ok, err := s.Next()
			if err != nil || !ok || !yield(entry) {
				return
			}
		}
	}
}

// Seq2 is Go 1.23 range-compatible and yields traversal errors explicitly as
// the second range value. A nil error accompanies every entry.
func (s *EntryStream) Seq2() iter.Seq2[SnapshotEntry, error] {
	return func(yield func(SnapshotEntry, error) bool) {
		defer s.Close()
		for {
			entry, ok, err := s.Next()
			if err != nil {
				yield(SnapshotEntry{}, err)
				return
			}
			if !ok || !yield(entry, nil) {
				return
			}
		}
	}
}

// SnapshotEntries materializes one immutable revision into host-owned keys.
func (d *Dictionary) SnapshotEntries() (EntrySnapshot, error) {
	stream, err := d.OpenEntryStream(DefaultEntryBatchLimits)
	if err != nil {
		return EntrySnapshot{}, err
	}
	snapshot := EntrySnapshot{Info: stream.Info()}
	if snapshot.Info.HasExactLen {
		snapshot.Entries = make([]SnapshotEntry, 0, snapshot.Info.ExactLen)
	}
	for {
		entry, ok, nextErr := stream.Next()
		if nextErr != nil {
			return EntrySnapshot{}, errors.Join(nextErr, stream.Close())
		}
		if !ok {
			return snapshot, stream.Close()
		}
		snapshot.Entries = append(snapshot.Entries, entry)
	}
}

// Entries returns the materialized entries of one immutable revision.
func (d *Dictionary) Entries() ([]SnapshotEntry, error) {
	snapshot, err := d.SnapshotEntries()
	return snapshot.Entries, err
}

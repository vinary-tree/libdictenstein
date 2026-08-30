package conformance

import (
	"math"
	"reflect"
	"testing"

	ld "github.com/vinary-tree/libdictenstein/bindings/go/v4"
)

func TestEntryCollectionsSnapshotDomainsAndValues(t *testing.T) {
	dictionary, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dictionary.Close()
	for _, item := range []struct {
		term  string
		value *uint64
	}{{"", nil}, {"a", id(0)}, {"é", id(math.MaxUint64)}} {
		_, err = dictionary.Put(item.term, item.value)
		must(t, err)
	}

	stream, err := dictionary.OpenEntryStream(ld.EntryBatchLimits{
		MaxEntries: 1, MaxUnits: 8, MaxValues: 1,
	})
	must(t, err)
	if stream.Info().UnitDomain != ld.UnicodeScalarDomain ||
		!stream.Info().HasExactLen || stream.Info().ExactLen != 3 {
		t.Fatalf("unexpected stream info: %#v", stream.Info())
	}
	_, err = dictionary.Put("later", id(7))
	must(t, err)
	fresh, err := dictionary.SnapshotEntries()
	must(t, err)
	if len(fresh.Entries) != 4 || fresh.Entries[2].Text != "later" {
		t.Fatalf("fresh materialized snapshot = %#v", fresh.Entries)
	}
	must(t, dictionary.Close())

	var got []ld.SnapshotEntry
	for entry, rangeErr := range stream.Seq2() {
		must(t, rangeErr)
		got = append(got, entry)
	}
	if terms := []string{got[0].Text, got[1].Text, got[2].Text}; !reflect.DeepEqual(terms, []string{"", "a", "é"}) {
		t.Fatalf("snapshot terms = %#v", terms)
	}
	if got[0].Value != nil || got[1].Value == nil || *got[1].Value != 0 ||
		got[2].Value == nil || *got[2].Value != math.MaxUint64 {
		t.Fatalf("snapshot values lost tri-state semantics: %#v", got)
	}

	bytes, err := ld.NewDynamicDawg(ld.ByteDomain)
	must(t, err)
	defer bytes.Close()
	raw := string([]byte{0, 0xff})
	_, err = bytes.Put(raw, nil)
	must(t, err)
	byteEntries, err := bytes.Entries()
	must(t, err)
	if len(byteEntries) != 1 || byteEntries[0].Domain != ld.ByteDomain ||
		!reflect.DeepEqual(byteEntries[0].Bytes, []byte{0, 0xff}) || byteEntries[0].Value != nil {
		t.Fatalf("byte entries = %#v", byteEntries)
	}

	tokens, err := ld.NewDynamicDawg(ld.U64Domain)
	must(t, err)
	defer tokens.Close()
	_, err = tokens.PutU64([]uint64{1, math.MaxUint64}, id(0))
	must(t, err)
	tokenEntries, err := tokens.Entries()
	must(t, err)
	if len(tokenEntries) != 1 || tokenEntries[0].Domain != ld.U64Domain ||
		!reflect.DeepEqual(tokenEntries[0].U64, []uint64{1, math.MaxUint64}) ||
		tokenEntries[0].Value == nil || *tokenEntries[0].Value != 0 {
		t.Fatalf("u64 entries = %#v", tokenEntries)
	}
}

func TestNativeDictionaryAlgebra(t *testing.T) {
	left, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer left.Close()
	right, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer right.Close()
	_, err = left.PutAll([]ld.Entry{{Term: "a", Value: id(1)}, {Term: "shared", Value: id(7)}, {Term: "valueless"}})
	must(t, err)
	_, err = right.PutAll([]ld.Entry{{Term: "b", Value: id(2)}, {Term: "shared", Value: id(11)}, {Term: "valueless", Value: id(5)}})
	must(t, err)

	joined, err := left.UnionWith(right.Dictionary, ld.LatticeJoinValue)
	must(t, err)
	defer joined.Close()
	common, err := left.Intersection(right.Dictionary)
	must(t, err)
	defer common.Close()
	onlyLeft, err := left.Difference(right.Dictionary)
	must(t, err)
	defer onlyLeft.Close()
	exclusive, err := left.SymmetricDifference(right.Dictionary)
	must(t, err)
	defer exclusive.Close()

	if size, sizeErr := joined.Len(); sizeErr != nil || size != 4 {
		t.Fatalf("union size = %d, %v", size, sizeErr)
	}
	shared, err := joined.Get("shared")
	must(t, err)
	valueless, err := joined.Get("valueless")
	must(t, err)
	if shared.Value == nil || *shared.Value != 11 || valueless.Value == nil || *valueless.Value != 5 {
		t.Fatalf("union values = %#v, %#v", shared, valueless)
	}
	shared, err = common.Get("shared")
	must(t, err)
	valueless, err = common.Get("valueless")
	must(t, err)
	if shared.Value == nil || *shared.Value != 7 || valueless.Value != nil {
		t.Fatalf("intersection values = %#v, %#v", shared, valueless)
	}
	if present, containsErr := onlyLeft.Contains("a"); containsErr != nil || !present {
		t.Fatalf("difference lacks a: %v", containsErr)
	}
	if size, sizeErr := exclusive.Len(); sizeErr != nil || size != 2 {
		t.Fatalf("symmetric difference size = %d, %v", size, sizeErr)
	}
	_, err = left.Put("later", id(99))
	must(t, err)
	if present, containsErr := joined.Contains("later"); containsErr != nil || present {
		t.Fatalf("snapshot union observed later mutation: %v", containsErr)
	}
	inserted, err := joined.Put("mutable-result", id(23))
	must(t, err)
	if !inserted {
		t.Fatal("algebra result was not mutable")
	}
}

func TestEntrySeqEarlyBreakAndExplicitCancel(t *testing.T) {
	dictionary, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dictionary.Close()
	for _, term := range []string{"a", "b", "c"} {
		_, err = dictionary.Put(term, nil)
		must(t, err)
	}

	stream, err := dictionary.OpenEntryStream(ld.EntryBatchLimits{
		MaxEntries: 3, MaxUnits: 3, MaxValues: 0,
	})
	must(t, err)
	seen := 0
	for range stream.Seq() {
		seen++
		break
	}
	if seen != 1 || stream.Err() != nil {
		t.Fatalf("early-break stream: seen=%d err=%v", seen, stream.Err())
	}
	if _, ok, nextErr := stream.Next(); ok || nextErr != nil {
		t.Fatalf("closed stream Next = ok %v, err %v", ok, nextErr)
	}

	stream, err = dictionary.OpenEntryStream(ld.DefaultEntryBatchLimits)
	must(t, err)
	if _, ok, nextErr := stream.Next(); !ok || nextErr != nil {
		t.Fatalf("initial Next = ok %v, err %v", ok, nextErr)
	}
	must(t, stream.Cancel())
	must(t, stream.Cancel())
	if _, ok, nextErr := stream.Next(); ok || nextErr != nil {
		t.Fatalf("cancelled stream Next = ok %v, err %v", ok, nextErr)
	}
}

package libdictenstein

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"

	llev "github.com/vinary-tree/liblevenshtein-rust/bindings/go/v4"
)

func id(value uint64) *uint64 { return &value }
func must(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatal(err)
	}
}

func collect(t *testing.T, iterator *llev.Iterator) map[string]bool {
	t.Helper()
	result := map[string]bool{}
	for {
		match, ok, err := iterator.Next()
		must(t, err)
		if !ok {
			break
		}
		result[match.Text] = true
	}
	return result
}

func TestQueryStartSnapshotAcrossMutations(t *testing.T) {
	dictionary, err := NewDynamicDawg(UnicodeScalarDomain)
	must(t, err)
	defer dictionary.Close()
	_, err = dictionary.PutAll([]Entry{{"cat", id(1)}, {"cot", id(2)}, {"cut", id(3)}, {"scat", nil}})
	must(t, err)
	transducer, err := llev.NewTransducer(dictionary, llev.Standard)
	must(t, err)
	defer transducer.Close()
	iterator, err := transducer.Query("cat", 2, llev.Traversal)
	must(t, err)
	first, ok, err := iterator.Next()
	must(t, err)
	if !ok {
		t.Fatal("empty iterator")
	}
	old := map[string]bool{first.Text: true}
	_, err = dictionary.Remove("cot")
	must(t, err)
	_, err = dictionary.Put("cut", id(30))
	must(t, err)
	_, err = dictionary.Put("cit", id(5))
	must(t, err)
	_, err = dictionary.Compact()
	must(t, err)
	for {
		match, more, nextErr := iterator.Next()
		must(t, nextErr)
		if !more {
			break
		}
		old[match.Text] = true
	}
	for _, term := range []string{"cat", "cot", "cut", "scat"} {
		if !old[term] {
			t.Fatalf("old snapshot lacks %s: %#v", term, old)
		}
	}
	if old["cit"] {
		t.Fatal("old snapshot observed insertion")
	}

	for seed := 0; seed < 64; seed++ {
		subject, createErr := NewDynamicDawg(UnicodeScalarDomain)
		must(t, createErr)
		for index := 0; index < 32; index++ {
			_, putErr := subject.Put(fmt.Sprintf("t%02d", index), id(uint64(index)))
			must(t, putErr)
		}
		machine, machineErr := llev.NewTransducer(subject, llev.Standard)
		must(t, machineErr)
		baseline, queryErr := machine.Query("t00", 3, llev.Traversal)
		must(t, queryErr)
		expected := collect(t, baseline)
		stream, queryErr := machine.Query("t00", 3, llev.Traversal)
		must(t, queryErr)
		head, exists, nextErr := stream.Next()
		must(t, nextErr)
		if !exists {
			t.Fatal("property cursor empty")
		}
		_, removeErr := subject.Remove(fmt.Sprintf("t%02d", seed%32))
		must(t, removeErr)
		_, putErr := subject.Put(fmt.Sprintf("x%02d", seed), id(uint64(seed)))
		must(t, putErr)
		actual := map[string]bool{head.Text: true}
		for {
			match, more, nextErr := stream.Next()
			must(t, nextErr)
			if !more {
				break
			}
			actual[match.Text] = true
		}
		if len(actual) != len(expected) {
			t.Fatalf("seed %d snapshot size changed", seed)
		}
		for term := range expected {
			if !actual[term] {
				t.Fatalf("seed %d lacks %s", seed, term)
			}
		}
		machine.Close()
		subject.Close()
	}
}

func TestOtherBackends(t *testing.T) {
	dat, err := NewDoubleArrayTrie([]Entry{{"alpha", id(7)}, {"beta", nil}}, UnicodeScalarDomain)
	must(t, err)
	defer dat.Close()
	lookup, err := dat.Get("alpha")
	must(t, err)
	if lookup.Value == nil || *lookup.Value != 7 {
		t.Fatal("DAT lookup")
	}
	suffix, err := NewScdawg(UnicodeScalarDomain)
	must(t, err)
	defer suffix.Close()
	_, err = suffix.Put("banana", id(1))
	must(t, err)
	contains, err := suffix.ContainsSubstring("ana")
	must(t, err)
	frequency, err := suffix.SubstringFrequency("ana")
	must(t, err)
	if !contains || frequency != 2 {
		t.Fatal("SCDAWG substring")
	}
	path := filepath.Join(t.TempDir(), "dictionary")
	persistent, err := CreatePersistentArtrie(path, UnicodeScalarDomain)
	must(t, err)
	_, err = persistent.Put("durable", id(9))
	must(t, err)
	must(t, persistent.Checkpoint())
	must(t, persistent.Close())
	reopened, err := OpenPersistentArtrie(path, UnicodeScalarDomain)
	must(t, err)
	value, err := reopened.Get("durable")
	must(t, err)
	if value.Value == nil || *value.Value != 9 {
		t.Fatal("persistent reopen")
	}
	reopened.Close()
	_ = os.Remove(path + ".wlock")
}

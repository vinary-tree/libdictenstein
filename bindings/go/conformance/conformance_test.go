// Package conformance instantiates the uniform family C1-C10 facade contract
// for the Go binding against a live libdictenstein shared library.
//
// It is deliberately a *separate* package from the cross-project snapshot test
// in the parent directory: this suite needs only libdictenstein and the
// canonical fixture, never a liblevenshtein transducer, so it pins the
// *producer* ABI in isolation and builds without the consumer cdylib.
//
//	C1  identity/version           TestC1_*
//	C2  lifecycle/ownership        TestC2_*   (idempotent close + free order)
//	C3  error-mapping matrix       TestC3_*   (reachable LdictStatus arms + msg)
//	C4  canonical fixture replay   TestC4_*   (cross-language oracle)
//	C5  CRUD/value/batch/substring TestC5_*   (+ capability-derived rejects)
//	C6  text domains / values      TestC6_*   (é/🦀/combining/NUL/invalid/u64)
//	C7  batch edges                TestC7_*   (0/1/255/256/257/large)
//	C8  property vs oracle         TestC8_*   (CRUD script + substring naive)
//	C9  leak discipline            TestC9_*   (>=10k cycles, RSS bounded)
//	C10 concurrency                TestC10_*  (parallel snapshot/mutate)
//
// Run (with the shared library on the loader/linker search path), e.g.:
//
//	REL=../../../target/release CGO_LDFLAGS="-L$REL" LD_LIBRARY_PATH="$REL" \
//	  go test ./conformance/
package conformance

import (
	"bufio"
	"encoding/json"
	"errors"
	"math"
	"math/rand"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"

	ld "github.com/vinary-tree/libdictenstein/bindings/go/v4"
)

// ---------------------------------------------------------------------------
// Fixture (contract C4)
// ---------------------------------------------------------------------------

type fixtureEntry struct {
	Term  string  `json:"term"`
	Value *uint64 `json:"value"`
}

type fixture struct {
	UnitDomain string         `json:"unit_domain"`
	Entries    []fixtureEntry `json:"entries"`
	Size       int            `json:"size"`
	Contains   []struct {
		Term     string `json:"term"`
		Expected bool   `json:"expected"`
	} `json:"contains"`
	Get []struct {
		Term  string  `json:"term"`
		Found bool    `json:"found"`
		Value *uint64 `json:"value"`
	} `json:"get"`
	SubstringFrequency []struct {
		Pattern  string `json:"pattern"`
		Expected uint   `json:"expected"`
	} `json:"substring_frequency"`
	SubstringContains []struct {
		Pattern  string `json:"pattern"`
		Expected bool   `json:"expected"`
	} `json:"substring_contains"`
}

func loadFixture(t *testing.T) fixture {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "..", "canonical_fixture.json"))
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	var parsed fixture
	if err := json.Unmarshal(raw, &parsed); err != nil {
		t.Fatalf("parse fixture: %v", err)
	}
	return parsed
}

func (f fixture) entries() []ld.Entry {
	result := make([]ld.Entry, 0, len(f.Entries))
	for _, item := range f.Entries {
		result = append(result, ld.Entry{Term: item.Term, Value: item.Value})
	}
	return result
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

func id(value uint64) *uint64 { v := value; return &v }

func must(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatal(err)
	}
}

func equalPtr(a, b *uint64) bool {
	switch {
	case a == nil && b == nil:
		return true
	case a == nil || b == nil:
		return false
	default:
		return *a == *b
	}
}

// statusOf extracts the native LdictStatus from a facade error, or 0 if the
// error is not a native status (e.g. a Go-level guard).
func statusOf(err error) uint32 {
	var native *ld.Error
	if errors.As(err, &native) {
		return native.Status
	}
	return 0
}

// rssKiB reads the resident set size (VmRSS) from /proc/self/status. Returns 0
// if unavailable (non-Linux); the leak test degrades to a no-op there.
func rssKiB(t *testing.T) uint64 {
	file, err := os.Open("/proc/self/status")
	if err != nil {
		return 0
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := scanner.Text()
		if strings.HasPrefix(line, "VmRSS:") {
			fields := strings.Fields(line)
			if len(fields) >= 2 {
				value, _ := strconv.ParseUint(fields[1], 10, 64)
				return value
			}
		}
	}
	return 0
}

// ---------------------------------------------------------------------------
// C1 identity/version
// ---------------------------------------------------------------------------

func TestC1_IdentityConstants(t *testing.T) {
	if ld.AbiVersion() != 1 {
		t.Fatalf("abi version = %d, want 1", ld.AbiVersion())
	}
	if ld.ApiRevision() != 5 {
		t.Fatalf("api revision = %d, want 5", ld.ApiRevision())
	}
}

func TestC1_KindAndCapabilities(t *testing.T) {
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	kind, err := dawg.Kind()
	must(t, err)
	if kind != ld.DynamicDawgKind {
		t.Fatalf("dawg kind = %d", kind)
	}
	caps, err := dawg.Capabilities()
	must(t, err)
	for _, bit := range []uint64{ld.CanInsert, ld.CanRemove, ld.CanClear, ld.CanCompact} {
		if caps&bit == 0 {
			t.Fatalf("dawg missing capability bit %d", bit)
		}
	}
	if caps&ld.CanSubstring != 0 || caps&ld.CanCheckpoint != 0 {
		t.Fatal("dawg advertises substring/checkpoint it does not support")
	}

	dat, err := ld.NewDoubleArrayTrie([]ld.Entry{{Term: "x"}}, ld.UnicodeScalarDomain)
	must(t, err)
	defer dat.Close()
	kind, err = dat.Kind()
	must(t, err)
	if kind != ld.DoubleArrayTrieKind {
		t.Fatalf("dat kind = %d", kind)
	}
	caps, err = dat.Capabilities()
	must(t, err)
	if caps&ld.CanRead == 0 {
		t.Fatal("dat missing read")
	}
	if caps&ld.CanInsert != 0 || caps&ld.CanClear != 0 {
		t.Fatal("dat advertises mutation it does not support")
	}

	scdawg, err := ld.NewScdawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer scdawg.Close()
	kind, err = scdawg.Kind()
	must(t, err)
	if kind != ld.ScdawgKind {
		t.Fatalf("scdawg kind = %d", kind)
	}
	caps, err = scdawg.Capabilities()
	must(t, err)
	if caps&ld.CanSubstring == 0 {
		t.Fatal("scdawg missing substring")
	}
}

// ---------------------------------------------------------------------------
// C2 lifecycle/ownership
// ---------------------------------------------------------------------------

func TestC2_DoubleCloseIsIdempotent(t *testing.T) {
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	if _, err := dawg.Put("a", nil); err != nil {
		t.Fatal(err)
	}
	must(t, dawg.Close())
	must(t, dawg.Close()) // no double free, no crash
}

func TestC2_FreeOrderIndependence(t *testing.T) {
	dicts := make([]*ld.DynamicDawg, 4)
	for i := range dicts {
		d, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
		must(t, err)
		if _, err := d.Put("term"+strconv.Itoa(i), id(uint64(i))); err != nil {
			t.Fatal(err)
		}
		dicts[i] = d
	}
	// Free in an order unrelated to construction order.
	for _, index := range []int{2, 0, 3, 1} {
		must(t, dicts[index].Close())
	}
}

// ---------------------------------------------------------------------------
// C3 error-mapping matrix + thread-local message
//
// Reachable through the idiomatic typed API: INVALID_UTF8 (3),
// DOMAIN_MISMATCH (9), IO_ERROR (7). The remaining arms are marked N/A:
//   - NULL_POINTER (4):   the facade guards a closed handle with a Go-level
//                         error before crossing the ABI, so the native null
//                         path is unreachable idiomatically.
//   - UNSUPPORTED (6):    no typed method exposes an operation the backend
//                         does not advertise; capability bits are asserted
//                         absent instead (see C5).
//   - LIMIT_EXCEEDED (10):PersistentVocabulary.Term auto-sizes its buffer with
//                         the documented two-call pattern, so truncation never
//                         surfaces to the caller.
// ---------------------------------------------------------------------------

func TestC3_InvalidUtf8(t *testing.T) {
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	_, err = dawg.Put(string([]byte{0xFF}), nil)
	if got := statusOf(err); got != 3 {
		t.Fatalf("invalid utf-8 status = %d, want 3 (err=%v)", got, err)
	}
	if err.Error() == "" {
		t.Fatal("empty error message")
	}
}

func TestC3_DomainMismatch(t *testing.T) {
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	_, err = dawg.PutU64([]uint64{1, 2}, nil)
	if got := statusOf(err); got != 9 {
		t.Fatalf("domain mismatch status = %d, want 9 (err=%v)", got, err)
	}
}

func TestC3_IoErrorOnMissingPersistent(t *testing.T) {
	_, err := ld.OpenPersistentArtrie(filepath.Join(t.TempDir(), "does-not-exist.part"), ld.UnicodeScalarDomain)
	if got := statusOf(err); got != 7 {
		t.Fatalf("io error status = %d, want 7 (err=%v)", got, err)
	}
}

// ---------------------------------------------------------------------------
// C4 canonical fixture replay (cross-language oracle)
// ---------------------------------------------------------------------------

type reader interface {
	Len() (uint, error)
	Contains(string) (bool, error)
	Get(string) (ld.Lookup, error)
}

func assertFixtureReads(t *testing.T, f fixture, d reader) {
	t.Helper()
	length, err := d.Len()
	must(t, err)
	if int(length) != f.Size {
		t.Fatalf("len = %d, want %d", length, f.Size)
	}
	for _, item := range f.Contains {
		got, err := d.Contains(item.Term)
		must(t, err)
		if got != item.Expected {
			t.Fatalf("contains(%q) = %v, want %v", item.Term, got, item.Expected)
		}
	}
	for _, item := range f.Get {
		lookup, err := d.Get(item.Term)
		must(t, err)
		if lookup.Found != item.Found {
			t.Fatalf("get(%q).found = %v, want %v", item.Term, lookup.Found, item.Found)
		}
		if !equalPtr(lookup.Value, item.Value) {
			t.Fatalf("get(%q).value mismatch", item.Term)
		}
	}
}

func TestC4_DynamicDawgMatchesOracle(t *testing.T) {
	f := loadFixture(t)
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	inserted, err := dawg.PutAll(f.entries())
	must(t, err)
	if int(inserted) != f.Size {
		t.Fatalf("inserted = %d, want %d", inserted, f.Size)
	}
	assertFixtureReads(t, f, dawg)
}

func TestC4_DoubleArrayTrieMatchesOracle(t *testing.T) {
	f := loadFixture(t)
	dat, err := ld.NewDoubleArrayTrie(f.entries(), ld.UnicodeScalarDomain)
	must(t, err)
	defer dat.Close()
	assertFixtureReads(t, f, dat)
}

func TestC4_PersistentArtrieMatchesOracle(t *testing.T) {
	f := loadFixture(t)
	path := filepath.Join(t.TempDir(), "terms.part")
	art, err := ld.CreatePersistentArtrie(path, ld.UnicodeScalarDomain)
	must(t, err)
	defer art.Close()
	for _, item := range f.Entries {
		if _, err := art.Put(item.Term, item.Value); err != nil {
			t.Fatal(err)
		}
	}
	assertFixtureReads(t, f, art)
}

func TestC4_ScdawgMatchesSubstringOracle(t *testing.T) {
	f := loadFixture(t)
	scdawg, err := ld.NewScdawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer scdawg.Close()
	for _, item := range f.Entries {
		if _, err := scdawg.Put(item.Term, item.Value); err != nil {
			t.Fatal(err)
		}
	}
	for _, item := range f.SubstringFrequency {
		got, err := scdawg.SubstringFrequency(item.Pattern)
		must(t, err)
		if got != item.Expected {
			t.Fatalf("frequency(%q) = %d, want %d", item.Pattern, got, item.Expected)
		}
	}
	for _, item := range f.SubstringContains {
		got, err := scdawg.ContainsSubstring(item.Pattern)
		must(t, err)
		if got != item.Expected {
			t.Fatalf("contains_substring(%q) = %v, want %v", item.Pattern, got, item.Expected)
		}
	}
}

// ---------------------------------------------------------------------------
// C5 CRUD + value + batch + substring; capability-derived rejects
// ---------------------------------------------------------------------------

func TestC5_CrudRoundTrip(t *testing.T) {
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	changed, err := dawg.Put("cat", id(1))
	must(t, err)
	if !changed {
		t.Fatal("first insert should change")
	}
	changed, err = dawg.Put("cat", id(1))
	must(t, err)
	if changed {
		t.Fatal("idempotent insert should not change")
	}
	lookup, err := dawg.Get("cat")
	must(t, err)
	if !lookup.Found || !equalPtr(lookup.Value, id(1)) {
		t.Fatal("get cat")
	}
	removed, err := dawg.Remove("cat")
	must(t, err)
	if !removed {
		t.Fatal("remove cat")
	}
	removed, err = dawg.Remove("cat")
	must(t, err)
	if removed {
		t.Fatal("second remove should not change")
	}
}

func TestC5_CompactPreservesTerms(t *testing.T) {
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	batch := make([]ld.Entry, 50)
	for i := range batch {
		batch[i] = ld.Entry{Term: "t" + strconv.Itoa(i), Value: id(uint64(i))}
	}
	if _, err := dawg.PutAll(batch); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 50; i += 2 {
		if _, err := dawg.Remove("t" + strconv.Itoa(i)); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := dawg.Compact(); err != nil {
		t.Fatal(err)
	}
	length, err := dawg.Len()
	must(t, err)
	if length != 25 {
		t.Fatalf("len after compact = %d, want 25", length)
	}
	lookup, err := dawg.Get("t1")
	must(t, err)
	if !lookup.Found || !equalPtr(lookup.Value, id(1)) {
		t.Fatal("t1 survived compact")
	}
	present, err := dawg.Contains("t0")
	must(t, err)
	if present {
		t.Fatal("t0 should be gone")
	}
}

func TestC5_SubstringUpdatesWithInserts(t *testing.T) {
	scdawg, err := ld.NewScdawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer scdawg.Close()
	if _, err := scdawg.Put("cat", id(1)); err != nil {
		t.Fatal(err)
	}
	if _, err := scdawg.Put("cot", id(2)); err != nil {
		t.Fatal(err)
	}
	freq, err := scdawg.SubstringFrequency("t")
	must(t, err)
	if freq != 2 {
		t.Fatalf("frequency t = %d, want 2", freq)
	}
	if _, err := scdawg.Put("cut", nil); err != nil {
		t.Fatal(err)
	}
	freq, err = scdawg.SubstringFrequency("t")
	must(t, err)
	if freq != 3 {
		t.Fatalf("frequency t = %d, want 3", freq)
	}
}

func TestC5_CapabilityDerivedRejects(t *testing.T) {
	// DoubleArrayTrie advertises READ only: assert mutation bits are absent.
	dat, err := ld.NewDoubleArrayTrie([]ld.Entry{{Term: "x"}}, ld.UnicodeScalarDomain)
	must(t, err)
	defer dat.Close()
	caps, err := dat.Capabilities()
	must(t, err)
	for _, bit := range []uint64{ld.CanInsert, ld.CanRemove, ld.CanClear, ld.CanCompact} {
		if caps&bit != 0 {
			t.Fatalf("DAT advertises unsupported capability bit %d", bit)
		}
	}
	// DomainMismatch is the reachable capability-derived reject (u64 op on a
	// Unicode-scalar backend).
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	if _, err := dawg.PutU64([]uint64{1}, nil); statusOf(err) != 9 {
		t.Fatalf("expected DOMAIN_MISMATCH, got %v", err)
	}
}

// ---------------------------------------------------------------------------
// C6 text domains and values
// ---------------------------------------------------------------------------

func TestC6_PrecomposedAndMultibyte(t *testing.T) {
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	if _, err := dawg.Put("café", id(7)); err != nil { // precomposed U+00E9
		t.Fatal(err)
	}
	if _, err := dawg.Put("🦀", id(255)); err != nil { // 4-byte scalar
		t.Fatal(err)
	}
	present, err := dawg.Contains("café")
	must(t, err)
	if !present {
		t.Fatal("café absent")
	}
	lookup, err := dawg.Get("🦀")
	must(t, err)
	if !lookup.Found || !equalPtr(lookup.Value, id(255)) {
		t.Fatal("🦀 value")
	}
}

func TestC6_CombiningDistinctFromPrecomposed(t *testing.T) {
	precomposed := "café" // café with precomposed U+00E9
	combining := "café"  // cafe + U+0301 combining acute
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	if _, err := dawg.Put(precomposed, id(1)); err != nil {
		t.Fatal(err)
	}
	if _, err := dawg.Put(combining, id(2)); err != nil {
		t.Fatal(err)
	}
	length, err := dawg.Len()
	must(t, err)
	if length != 2 {
		t.Fatalf("len = %d, want 2 (distinct scalar sequences)", length)
	}
	p, err := dawg.Get(precomposed)
	must(t, err)
	c, err := dawg.Get(combining)
	must(t, err)
	if !equalPtr(p.Value, id(1)) || !equalPtr(c.Value, id(2)) {
		t.Fatal("distinct values")
	}
}

func TestC6_ByteDomainAcceptsNulAndInvalidUtf8(t *testing.T) {
	dawg, err := ld.NewDynamicDawg(ld.ByteDomain)
	must(t, err)
	defer dawg.Close()
	embeddedNul := string([]byte{'a', 0x00, 'b'})
	invalidUtf8 := string([]byte{0xFF, 0xFE})
	if _, err := dawg.Put(embeddedNul, id(1)); err != nil {
		t.Fatal(err)
	}
	if _, err := dawg.Put(invalidUtf8, id(2)); err != nil {
		t.Fatal(err)
	}
	present, err := dawg.Contains(embeddedNul)
	must(t, err)
	if !present {
		t.Fatal("embedded NUL term absent")
	}
	lookup, err := dawg.Get(invalidUtf8)
	must(t, err)
	if !lookup.Found || !equalPtr(lookup.Value, id(2)) {
		t.Fatal("invalid utf-8 byte term")
	}
}

func TestC6_U64DomainValuesZeroAndMax(t *testing.T) {
	dawg, err := ld.NewDynamicDawg(ld.U64Domain)
	must(t, err)
	defer dawg.Close()
	if _, err := dawg.PutU64([]uint64{1, 2, 3}, id(0)); err != nil {
		t.Fatal(err)
	}
	if _, err := dawg.PutU64([]uint64{9}, id(math.MaxUint64)); err != nil {
		t.Fatal(err)
	}
	a, err := dawg.GetU64([]uint64{1, 2, 3})
	must(t, err)
	if !a.Found || !equalPtr(a.Value, id(0)) {
		t.Fatal("u64 value 0")
	}
	b, err := dawg.GetU64([]uint64{9})
	must(t, err)
	if !b.Found || !equalPtr(b.Value, id(math.MaxUint64)) {
		t.Fatal("u64 value MAX")
	}
}

// ---------------------------------------------------------------------------
// C7 batch / paging edges
// ---------------------------------------------------------------------------

func TestC7_BatchSizes(t *testing.T) {
	for _, size := range []int{0, 1, 255, 256, 257, 1000} {
		dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
		must(t, err)
		batch := make([]ld.Entry, size)
		for i := range batch {
			batch[i] = ld.Entry{Term: "t" + strconv.Itoa(i), Value: id(uint64(i))}
		}
		inserted, err := dawg.PutAll(batch)
		must(t, err)
		if int(inserted) != size {
			t.Fatalf("size %d: inserted %d", size, inserted)
		}
		length, err := dawg.Len()
		must(t, err)
		if int(length) != size {
			t.Fatalf("size %d: len %d", size, length)
		}
		if size > 0 {
			first, err := dawg.Get("t0")
			must(t, err)
			last, err := dawg.Get("t" + strconv.Itoa(size-1))
			must(t, err)
			if !equalPtr(first.Value, id(0)) || !equalPtr(last.Value, id(uint64(size-1))) {
				t.Fatalf("size %d: boundary values", size)
			}
		}
		dawg.Close()
	}
}

// ---------------------------------------------------------------------------
// C8 property-based testing vs an in-language oracle
// ---------------------------------------------------------------------------

func TestC8_CrudScriptMatchesMapOracle(t *testing.T) {
	rng := rand.New(rand.NewSource(0xC0FFEE))
	keys := make([]string, 40)
	for i := range keys {
		keys[i] = "k" + strconv.Itoa(i)
	}
	oracle := map[string]*uint64{}
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	for i := 0; i < 3000; i++ {
		key := keys[rng.Intn(len(keys))]
		_, present := oracle[key]
		switch op := rng.Float64(); {
		case op < 0.5:
			var value *uint64
			if rng.Intn(2) == 0 {
				value = id(uint64(rng.Int63()))
			}
			changed, err := dawg.Put(key, value)
			must(t, err)
			if changed != !present {
				t.Fatalf("insert changed=%v present=%v", changed, present)
			}
			oracle[key] = value
		case op < 0.75:
			changed, err := dawg.Remove(key)
			must(t, err)
			if changed != present {
				t.Fatalf("remove changed=%v present=%v", changed, present)
			}
			delete(oracle, key)
		case op < 0.95:
			got, err := dawg.Contains(key)
			must(t, err)
			if got != present {
				t.Fatalf("contains=%v present=%v", got, present)
			}
			if present {
				lookup, err := dawg.Get(key)
				must(t, err)
				if !lookup.Found || !equalPtr(lookup.Value, oracle[key]) {
					t.Fatal("get mismatch")
				}
			}
		default:
			if _, err := dawg.Compact(); err != nil {
				t.Fatal(err)
			}
		}
		length, err := dawg.Len()
		must(t, err)
		if int(length) != len(oracle) {
			t.Fatalf("len %d != oracle %d", length, len(oracle))
		}
	}
}

func TestC8_SubstringMatchesNaiveOracle(t *testing.T) {
	rng := rand.New(rand.NewSource(0x5CDA))
	alphabet := []rune("abcx")
	generate := func(maxLen int) string {
		n := rng.Intn(maxLen) + 1
		out := make([]rune, n)
		for i := range out {
			out[i] = alphabet[rng.Intn(len(alphabet))]
		}
		return string(out)
	}
	termSet := map[string]struct{}{}
	for len(termSet) < 60 {
		termSet[generate(6)] = struct{}{}
	}
	terms := make([]string, 0, len(termSet))
	for term := range termSet {
		terms = append(terms, term)
	}
	naive := func(pattern string) uint {
		var total uint
		for _, term := range terms {
			for start := 0; start+len(pattern) <= len(term); start++ {
				if term[start:start+len(pattern)] == pattern {
					total++
				}
			}
		}
		return total
	}
	scdawg, err := ld.NewScdawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer scdawg.Close()
	for _, term := range terms {
		if _, err := scdawg.Put(term, nil); err != nil {
			t.Fatal(err)
		}
	}
	for i := 0; i < 200; i++ {
		pattern := generate(3)
		want := naive(pattern)
		freq, err := scdawg.SubstringFrequency(pattern)
		must(t, err)
		if freq != want {
			t.Fatalf("frequency(%q) = %d, want %d", pattern, freq, want)
		}
		contains, err := scdawg.ContainsSubstring(pattern)
		must(t, err)
		if contains != (want > 0) {
			t.Fatalf("contains_substring(%q) = %v, want %v", pattern, contains, want > 0)
		}
	}
}

// ---------------------------------------------------------------------------
// C9 leak discipline
// ---------------------------------------------------------------------------

func TestC9_CreateUseFreeCyclesDoNotLeak(t *testing.T) {
	if testing.Short() {
		t.Skip("leak soak skipped in -short mode")
	}
	const cycles = 12000
	batch := []ld.Entry{{Term: "cat", Value: id(1)}, {Term: "cot", Value: id(2)}, {Term: "cut"}}
	for warmup := 0; warmup < 2000; warmup++ { // reach allocator steady state
		dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
		must(t, err)
		if _, err := dawg.Put("cat", id(1)); err != nil {
			t.Fatal(err)
		}
		must(t, dawg.Close())
	}
	before := rssKiB(t)
	for i := 0; i < cycles; i++ {
		dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
		must(t, err)
		if _, err := dawg.PutAll(batch); err != nil {
			t.Fatal(err)
		}
		if present, err := dawg.Contains("cot"); err != nil || !present {
			t.Fatalf("cot absent (err=%v)", err)
		}
		must(t, dawg.Close())
	}
	after := rssKiB(t)
	if before != 0 && after > before && after-before > 32*1024 {
		t.Fatalf("RSS grew %d KiB over %d cycles", after-before, cycles)
	}
}

// ---------------------------------------------------------------------------
// C10 concurrency
// ---------------------------------------------------------------------------

func TestC10_IndependentDictionariesPerGoroutine(t *testing.T) {
	const workers = 8
	var wait sync.WaitGroup
	errs := make([]error, workers)
	for seed := 0; seed < workers; seed++ {
		wait.Add(1)
		go func(seed int) {
			defer wait.Done()
			dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
			if err != nil {
				errs[seed] = err
				return
			}
			defer dawg.Close()
			for i := 0; i < 2000; i++ {
				if _, err := dawg.Put("t"+strconv.Itoa(seed)+"_"+strconv.Itoa(i), id(uint64(i))); err != nil {
					errs[seed] = err
					return
				}
			}
			length, err := dawg.Len()
			if err != nil {
				errs[seed] = err
				return
			}
			if length != 2000 {
				errs[seed] = errors.New("len != 2000")
			}
		}(seed)
	}
	wait.Wait()
	for _, err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
}

func TestC10_ConcurrentReadersDuringWriter(t *testing.T) {
	dawg, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	must(t, err)
	defer dawg.Close()
	seed := make([]ld.Entry, 500)
	for i := range seed {
		seed[i] = ld.Entry{Term: "seed" + strconv.Itoa(i), Value: id(uint64(i))}
	}
	if _, err := dawg.PutAll(seed); err != nil {
		t.Fatal(err)
	}
	stop := make(chan struct{})
	var wait sync.WaitGroup
	readerErr := make(chan error, 4)
	for r := 0; r < 4; r++ {
		wait.Add(1)
		go func() {
			defer wait.Done()
			for {
				select {
				case <-stop:
					return
				default:
					if present, err := dawg.Contains("seed0"); err != nil || !present {
						readerErr <- errors.New("reader lost seed0")
						return
					}
					if _, err := dawg.Get("seed250"); err != nil {
						readerErr <- err
						return
					}
				}
			}
		}()
	}
	for i := 500; i < 3000; i++ {
		if _, err := dawg.Put("w"+strconv.Itoa(i), id(uint64(i))); err != nil {
			close(stop)
			wait.Wait()
			t.Fatal(err)
		}
	}
	close(stop)
	wait.Wait()
	select {
	case err := <-readerErr:
		t.Fatal(err)
	default:
	}
	final, err := dawg.Get("w2999")
	must(t, err)
	if !final.Found || !equalPtr(final.Value, id(2999)) {
		t.Fatal("final write missing")
	}
}

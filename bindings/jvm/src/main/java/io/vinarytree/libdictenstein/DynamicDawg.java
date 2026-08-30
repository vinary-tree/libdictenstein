package io.vinarytree.libdictenstein;

import static java.lang.foreign.ValueLayout.ADDRESS;

import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.OptionalLong;

/** Mutable DynamicDAWG with full CRUD and O(1) query-start revisions. */
public final class DynamicDawg extends Dictionary {
    /** Construct an empty Unicode-scalar DynamicDAWG. */
    public DynamicDawg() { this(UnitDomain.UNICODE_SCALAR); }

    /** Construct an empty dictionary in the selected unit domain. */
    public DynamicDawg(UnitDomain domain) { super(domain, create(domain)); }

    private DynamicDawg(UnitDomain domain, MemorySegment handle) { super(domain, handle); }

    static DynamicDawg adopt(UnitDomain domain, MemorySegment handle) {
        return new DynamicDawg(domain, handle);
    }

    private static MemorySegment create(UnitDomain domain) {
        Objects.requireNonNull(domain, "domain");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(ADDRESS);
            return newHandle(Native.create(domain.nativeValue, output), output);
        }
    }

    /** Insert or update one Unicode term. */
    public boolean put(String term, OptionalLong value) { return insert(term, value); }

    /** Insert or update one raw-byte term. */
    public boolean put(byte[] term, OptionalLong value) { return insert(term, value); }

    /** Insert or update one u64-token term. */
    public boolean put(long[] term, OptionalLong value) { return insert(term, value); }

    /** Remove one Unicode term. */
    public boolean remove(String term) { return removeTerm(term); }

    /** Remove one raw-byte term. */
    public boolean remove(byte[] term) { return removeTerm(term); }

    /** Remove one u64-token term. */
    public boolean remove(long[] term) { return removeTerm(term); }

    /** Insert/update UTF-8 strings with one managed/native crossing. */
    public long putAllStrings(Map<String, OptionalLong> entries) {
        return insertAllStrings(entries);
    }

    /** Insert/update byte terms with one managed/native crossing. */
    public long putAllBytes(List<TextEntry> entries) { return insertAllBytes(entries); }

    /** Insert/update u64-token terms with one managed/native crossing. */
    public long putAllU64(List<U64Entry> entries) { return insertAllU64(entries); }

    /** Publish a fresh empty revision. Existing cursors retain their revision. */
    public void clear() { clearTerms(); }

    /** Compact the current revision and return the reclaimed-node count. */
    public long compact() { return compactTerms(); }
}

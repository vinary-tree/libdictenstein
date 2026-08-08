package io.vinarytree.libdictenstein;

import static java.lang.foreign.ValueLayout.ADDRESS;

import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.OptionalLong;

/** Filesystem-backed byte, Unicode-scalar, or native-u64 adaptive radix trie. */
public final class PersistentARTrie extends Dictionary {
    private PersistentARTrie(Path path, UnitDomain domain, boolean create) {
        super(domain, load(path, domain, create));
    }

    /** Create a new persistent Unicode-scalar trie. */
    public static PersistentARTrie create(Path path) {
        return create(path, UnitDomain.UNICODE_SCALAR);
    }

    /** Create a new persistent trie. */
    public static PersistentARTrie create(Path path, UnitDomain domain) {
        return new PersistentARTrie(path, domain, true);
    }

    /** Open an existing persistent Unicode-scalar trie. */
    public static PersistentARTrie open(Path path) {
        return open(path, UnitDomain.UNICODE_SCALAR);
    }

    /** Open an existing persistent trie. */
    public static PersistentARTrie open(Path path, UnitDomain domain) {
        return new PersistentARTrie(path, domain, false);
    }

    private static MemorySegment load(Path path, UnitDomain domain, boolean create) {
        byte[] data = Objects.requireNonNull(path, "path").toString()
                .getBytes(StandardCharsets.UTF_8);
        Objects.requireNonNull(domain, "domain");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(ADDRESS);
            MemorySegment nativePath = bytes(arena, data);
            int status = create
                    ? Native.persistentCreate(domain.nativeValue, nativePath, data.length, output)
                    : Native.persistentOpen(domain.nativeValue, nativePath, data.length, output);
            return newHandle(status, output);
        }
    }

    /** Insert or update one Unicode term. */
    public boolean put(String term, OptionalLong value) { return insert(term, value); }

    /** Insert or update one raw-byte term. */
    public boolean put(byte[] term, OptionalLong value) { return insert(term, value); }

    /** Insert or update one native-u64 term. */
    public boolean put(long[] term, OptionalLong value) { return insert(term, value); }

    /** Remove one Unicode term. */
    public boolean remove(String term) { return removeTerm(term); }

    /** Remove one raw-byte term. */
    public boolean remove(byte[] term) { return removeTerm(term); }

    /** Remove one native-u64 term. */
    public boolean remove(long[] term) { return removeTerm(term); }

    /** Insert/update UTF-8 strings in one FFM crossing. */
    public long putAllStrings(Map<String, OptionalLong> entries) {
        return insertAllStrings(entries);
    }

    /** Insert/update byte terms in one FFM crossing. */
    public long putAllBytes(List<TextEntry> entries) { return insertAllBytes(entries); }

    /** Insert/update u64 terms in one FFM crossing. */
    public long putAllU64(List<U64Entry> entries) { return insertAllU64(entries); }

    /** Durably checkpoint the current published revision. */
    public void checkpoint() { Native.check(Native.checkpoint(handle())); }
}

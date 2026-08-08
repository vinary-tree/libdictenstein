package io.vinarytree.libdictenstein;

import static java.lang.foreign.ValueLayout.ADDRESS;

import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.Objects;
import java.util.OptionalLong;

/** Immutable cache-local DoubleArrayTrie. */
public final class DoubleArrayTrie extends Dictionary {
    /** Build a Unicode-scalar DAT from terms and optional metadata. */
    public DoubleArrayTrie(Map<String, OptionalLong> entries) {
        this(entries, UnitDomain.UNICODE_SCALAR);
    }

    /** Build a byte-transition or Unicode-scalar DAT in one FFM crossing. */
    public DoubleArrayTrie(Map<String, OptionalLong> entries, UnitDomain domain) {
        super(domain, create(entries, domain));
    }

    private static MemorySegment create(Map<String, OptionalLong> entries, UnitDomain domain) {
        Objects.requireNonNull(entries, "entries");
        Objects.requireNonNull(domain, "domain");
        if (domain == UnitDomain.U64) {
            throw new IllegalArgumentException("DoubleArrayTrie has no u64 variant");
        }
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment descriptors = arena.allocate(Native.ENTRY, Math.max(1, entries.size()));
            int index = 0;
            for (var item : entries.entrySet()) {
                byte[] term = Objects.requireNonNull(item.getKey(), "term")
                        .getBytes(StandardCharsets.UTF_8);
                writeEntry(descriptors.asSlice(index * Native.ENTRY.byteSize(),
                                Native.ENTRY.byteSize()),
                        bytes(arena, term), term.length,
                        Objects.requireNonNull(item.getValue(), "value"), arena);
                index++;
            }
            MemorySegment output = arena.allocate(ADDRESS);
            return newHandle(Native.createDat(
                    domain.nativeValue, descriptors, entries.size(), output), output);
        }
    }
}

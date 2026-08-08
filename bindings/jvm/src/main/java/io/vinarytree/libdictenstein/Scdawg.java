package io.vinarytree.libdictenstein;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_BYTE;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.OptionalLong;

/** Mutable SCDAWG with exact-term, suffix-language, and substring operations. */
public final class Scdawg extends Dictionary {
    /** Construct an empty Unicode-scalar SCDAWG. */
    public Scdawg() { this(UnitDomain.UNICODE_SCALAR); }

    /** Construct an empty byte-transition or Unicode-scalar SCDAWG. */
    public Scdawg(UnitDomain domain) { super(domain, create(domain)); }

    private static MemorySegment create(UnitDomain domain) {
        Objects.requireNonNull(domain, "domain");
        if (domain == UnitDomain.U64) throw new IllegalArgumentException("SCDAWG has no u64 variant");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(ADDRESS);
            return newHandle(Native.createScdawg(domain.nativeValue, output), output);
        }
    }

    /** Insert or update one Unicode term. */
    public boolean put(String term, OptionalLong value) { return insert(term, value); }

    /** Insert/update UTF-8 strings in one FFM crossing. */
    public long putAllStrings(Map<String, OptionalLong> entries) {
        return insertAllStrings(entries);
    }

    /** Insert/update raw byte descriptors in one FFM crossing. */
    public long putAllBytes(List<TextEntry> entries) { return insertAllBytes(entries); }

    /** Test whether a pattern is a substring of any indexed term. */
    public boolean containsSubstring(String pattern) {
        byte[] data = Objects.requireNonNull(pattern, "pattern").getBytes(StandardCharsets.UTF_8);
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_BYTE);
            Native.check(Native.containsSubstring(handle(), bytes(arena, data), data.length, output));
            return output.get(JAVA_BYTE, 0) != 0;
        }
    }

    /** Count a pattern's occurrences across indexed terms. */
    public long frequency(String pattern) {
        byte[] data = Objects.requireNonNull(pattern, "pattern").getBytes(StandardCharsets.UTF_8);
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_LONG);
            Native.check(Native.substringFrequency(handle(), bytes(arena, data), data.length, output));
            return output.get(JAVA_LONG, 0);
        }
    }
}

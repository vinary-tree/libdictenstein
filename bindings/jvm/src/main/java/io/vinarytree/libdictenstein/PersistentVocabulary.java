package io.vinarytree.libdictenstein;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_BYTE;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.Objects;
import java.util.Optional;
import java.util.OptionalLong;

/** Filesystem-backed bidirectional Unicode term/u64-index vocabulary. */
public final class PersistentVocabulary extends Dictionary {
    private PersistentVocabulary(Path path, boolean create) {
        super(UnitDomain.UNICODE_SCALAR, load(path, create));
    }

    /** Create a new persistent vocabulary. */
    public static PersistentVocabulary create(Path path) {
        return new PersistentVocabulary(path, true);
    }

    /** Open an existing persistent vocabulary. */
    public static PersistentVocabulary open(Path path) {
        return new PersistentVocabulary(path, false);
    }

    private static MemorySegment load(Path path, boolean create) {
        byte[] data = Objects.requireNonNull(path, "path").toString()
                .getBytes(StandardCharsets.UTF_8);
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(ADDRESS);
            MemorySegment nativePath = bytes(arena, data);
            int status = create
                    ? Native.vocabCreate(nativePath, data.length, output)
                    : Native.vocabOpen(nativePath, data.length, output);
            return newHandle(status, output);
        }
    }

    /** Insert a term at an explicit stable vocabulary index. */
    public boolean put(String term, long index) {
        return insert(term, OptionalLong.of(index));
    }

    /** Durably checkpoint the current vocabulary revision. */
    public void checkpoint() { Native.check(Native.checkpoint(handle())); }

    /** Look up a term by its stable vocabulary index. */
    public Optional<String> term(long index) {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment length = arena.allocate(JAVA_LONG);
            MemorySegment found = arena.allocate(JAVA_BYTE);
            Native.check(Native.vocabTerm(handle(), index, MemorySegment.NULL, 0, length, found));
            if (found.get(JAVA_BYTE, 0) == 0) return Optional.empty();
            long size = length.get(JAVA_LONG, 0);
            MemorySegment output = arena.allocate(Math.max(1, size), 1);
            Native.check(Native.vocabTerm(handle(), index, output, size, length, found));
            return Optional.of(new String(output.asSlice(0, size).toArray(JAVA_BYTE),
                    StandardCharsets.UTF_8));
        }
    }
}

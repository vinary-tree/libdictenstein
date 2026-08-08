package io.vinarytree.libdictenstein;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_BYTE;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

import io.vinarytree.interop.DictionaryResource;
import io.vinarytree.interop.InteropLayouts;
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.lang.ref.Cleaner;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.OptionalLong;

/** Common read-only surface for project-owned native dictionaries. */
public abstract class Dictionary implements DictionaryResource {
    private static final Cleaner CLEANER = Cleaner.create();

    private static final class State implements Runnable {
        private MemorySegment handle;
        private final Arena arena;

        State(MemorySegment handle, Arena arena) {
            this.handle = handle;
            this.arena = arena;
        }

        synchronized MemorySegment handle() {
            if (handle.equals(MemorySegment.NULL)) {
                throw new IllegalStateException("dictionary is closed");
            }
            return handle;
        }

        @Override public synchronized void run() {
            if (!handle.equals(MemorySegment.NULL)) {
                Native.free(handle);
                handle = MemorySegment.NULL;
                arena.close();
            }
        }
    }

    /** Lookup result preserving absent, valueless, and valued terms. */
    public record Lookup(boolean present, OptionalLong value) {
        public Lookup {
            Objects.requireNonNull(value, "value");
            if (!present && value.isPresent()) {
                throw new IllegalArgumentException("an absent term cannot have a value");
            }
        }
    }

    /** Raw byte term for one batched insert/update. */
    public record TextEntry(byte[] term, OptionalLong value) {
        public TextEntry {
            term = Objects.requireNonNull(term, "term").clone();
            Objects.requireNonNull(value, "value");
        }

        @Override public byte[] term() { return term.clone(); }
    }

    /** U64-token term for one batched insert/update. */
    public record U64Entry(long[] term, OptionalLong value) {
        public U64Entry {
            term = Objects.requireNonNull(term, "term").clone();
            Objects.requireNonNull(value, "value");
        }

        @Override public long[] term() { return term.clone(); }
    }

    private final UnitDomain domain;
    private final State state;
    private final Cleaner.Cleanable cleanable;
    private final MemorySegment resource;

    Dictionary(UnitDomain domain, MemorySegment handle) {
        this.domain = Objects.requireNonNull(domain, "domain");
        Arena owned = Arena.ofShared();
        state = new State(Objects.requireNonNull(handle, "handle"), owned);
        try {
            resource = owned.allocate(InteropLayouts.RESOURCE);
            Native.check(Native.resource(handle, resource));
            cleanable = CLEANER.register(this, state);
        } catch (Throwable failure) {
            state.run();
            throw failure;
        }
    }

    /** Unit domain fixed at construction/open time. */
    public final UnitDomain domain() { return domain; }

    /** Stable native backend identifier. */
    public final int kind() {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(java.lang.foreign.ValueLayout.JAVA_INT);
            Native.check(Native.kind(handle(), output));
            return output.get(java.lang.foreign.ValueLayout.JAVA_INT, 0);
        }
    }

    /** Native capability bitset. */
    public final long capabilities() {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_LONG);
            Native.check(Native.capabilities(handle(), output));
            return output.get(JAVA_LONG, 0);
        }
    }

    /** Number of visible terms. */
    public final long size() {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_LONG);
            Native.check(Native.len(handle(), output));
            return output.get(JAVA_LONG, 0);
        }
    }

    /** Test a Unicode term. */
    public final boolean contains(String term) {
        return containsText(Objects.requireNonNull(term, "term").getBytes(StandardCharsets.UTF_8));
    }

    /** Test a raw byte term. */
    public final boolean contains(byte[] term) {
        return containsText(Objects.requireNonNull(term, "term"));
    }

    /** Test a u64-token term. */
    public final boolean contains(long[] term) {
        ensureDomain(UnitDomain.U64);
        Objects.requireNonNull(term, "term");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_BYTE);
            Native.check(Native.containsU64(handle(), longs(arena, term), term.length, output));
            return booleanOutput(output);
        }
    }

    /** Read a Unicode term. */
    public final Lookup get(String term) {
        return getText(Objects.requireNonNull(term, "term").getBytes(StandardCharsets.UTF_8));
    }

    /** Read a raw byte term. */
    public final Lookup get(byte[] term) {
        return getText(Objects.requireNonNull(term, "term"));
    }

    /** Read a u64-token term. */
    public final Lookup get(long[] term) {
        ensureDomain(UnitDomain.U64);
        Objects.requireNonNull(term, "term");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment found = arena.allocate(JAVA_BYTE);
            MemorySegment value = arena.allocate(Native.OPTIONAL_U64);
            Native.check(Native.getU64(handle(), longs(arena, term), term.length, found, value));
            return lookup(found, value);
        }
    }

    @Override public final MemorySegment resourceSegment() {
        handle();
        return resource;
    }

    @Override public final void close() { cleanable.clean(); }

    protected final MemorySegment handle() { return state.handle(); }

    protected final boolean insert(String term, OptionalLong value) {
        return insert(Objects.requireNonNull(term, "term").getBytes(StandardCharsets.UTF_8), value);
    }

    protected final boolean insert(byte[] term, OptionalLong value) {
        ensureTextDomain();
        Objects.requireNonNull(term, "term");
        Objects.requireNonNull(value, "value");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_BYTE);
            Native.check(Native.insertText(handle(), bytes(arena, term), term.length,
                    optional(arena, value), output));
            return booleanOutput(output);
        }
    }

    protected final boolean insert(long[] term, OptionalLong value) {
        ensureDomain(UnitDomain.U64);
        Objects.requireNonNull(term, "term");
        Objects.requireNonNull(value, "value");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_BYTE);
            Native.check(Native.insertU64(handle(), longs(arena, term), term.length,
                    optional(arena, value), output));
            return booleanOutput(output);
        }
    }

    protected final boolean removeTerm(String term) {
        return removeTerm(Objects.requireNonNull(term, "term").getBytes(StandardCharsets.UTF_8));
    }

    protected final boolean removeTerm(byte[] term) {
        ensureTextDomain();
        Objects.requireNonNull(term, "term");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_BYTE);
            Native.check(Native.removeText(handle(), bytes(arena, term), term.length, output));
            return booleanOutput(output);
        }
    }

    protected final boolean removeTerm(long[] term) {
        ensureDomain(UnitDomain.U64);
        Objects.requireNonNull(term, "term");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_BYTE);
            Native.check(Native.removeU64(handle(), longs(arena, term), term.length, output));
            return booleanOutput(output);
        }
    }

    protected final long insertAllStrings(Map<String, OptionalLong> entries) {
        Objects.requireNonNull(entries, "entries");
        return insertAllBytes(entries.entrySet().stream()
                .map(entry -> new TextEntry(
                        Objects.requireNonNull(entry.getKey(), "term")
                                .getBytes(StandardCharsets.UTF_8),
                        Objects.requireNonNull(entry.getValue(), "value")))
                .toList());
    }

    protected final long insertAllBytes(List<TextEntry> entries) {
        ensureTextDomain();
        Objects.requireNonNull(entries, "entries");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment nativeEntries = arena.allocate(Native.ENTRY, Math.max(1, entries.size()));
            for (int index = 0; index < entries.size(); index++) {
                TextEntry entry = Objects.requireNonNull(entries.get(index), "entry");
                byte[] term = entry.term;
                writeEntry(nativeEntries.asSlice(index * Native.ENTRY.byteSize(),
                        Native.ENTRY.byteSize()), bytes(arena, term), term.length,
                        entry.value, arena);
            }
            MemorySegment output = arena.allocate(JAVA_LONG);
            Native.check(Native.insertTextBatch(handle(), nativeEntries, entries.size(), output));
            return output.get(JAVA_LONG, 0);
        }
    }

    protected final long insertAllU64(List<U64Entry> entries) {
        ensureDomain(UnitDomain.U64);
        Objects.requireNonNull(entries, "entries");
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment nativeEntries = arena.allocate(Native.ENTRY, Math.max(1, entries.size()));
            for (int index = 0; index < entries.size(); index++) {
                U64Entry entry = Objects.requireNonNull(entries.get(index), "entry");
                long[] term = entry.term;
                writeEntry(nativeEntries.asSlice(index * Native.ENTRY.byteSize(),
                        Native.ENTRY.byteSize()), longs(arena, term), term.length,
                        entry.value, arena);
            }
            MemorySegment output = arena.allocate(JAVA_LONG);
            Native.check(Native.insertU64Batch(handle(), nativeEntries, entries.size(), output));
            return output.get(JAVA_LONG, 0);
        }
    }

    protected final void clearTerms() { Native.check(Native.clear(handle())); }

    protected final long compactTerms() {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_LONG);
            Native.check(Native.compact(handle(), output));
            return output.get(JAVA_LONG, 0);
        }
    }

    static MemorySegment newHandle(int status, MemorySegment output) {
        Native.check(status);
        MemorySegment handle = output.get(ADDRESS, 0);
        if (handle.equals(MemorySegment.NULL)) {
            throw new IllegalStateException("native constructor returned a null handle");
        }
        return handle;
    }

    static MemorySegment bytes(Arena arena, byte[] value) {
        MemorySegment result = arena.allocate(Math.max(1, value.length), 1);
        if (value.length != 0) result.asSlice(0, value.length).copyFrom(MemorySegment.ofArray(value));
        return result;
    }

    static MemorySegment longs(Arena arena, long[] value) {
        MemorySegment result = arena.allocate(
                Math.max(1, value.length) * JAVA_LONG.byteSize(), JAVA_LONG.byteAlignment());
        for (int index = 0; index < value.length; index++) result.setAtIndex(JAVA_LONG, index, value[index]);
        return result;
    }

    static MemorySegment optional(Arena arena, OptionalLong value) {
        MemorySegment result = arena.allocate(Native.OPTIONAL_U64);
        result.fill((byte) 0);
        result.set(JAVA_LONG, Native.OPTIONAL_VALUE, value.orElse(0));
        result.set(JAVA_BYTE, Native.OPTIONAL_PRESENT, (byte) (value.isPresent() ? 1 : 0));
        return result;
    }

    static void writeEntry(MemorySegment entry, MemorySegment data, long length,
                           OptionalLong value, Arena arena) {
        entry.set(ADDRESS, Native.ENTRY_DATA, data);
        entry.set(JAVA_LONG, Native.ENTRY_LEN, length);
        entry.asSlice(Native.ENTRY_VALUE, Native.OPTIONAL_U64.byteSize())
                .copyFrom(optional(arena, value));
    }

    private boolean containsText(byte[] term) {
        ensureTextDomain();
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment output = arena.allocate(JAVA_BYTE);
            Native.check(Native.containsText(handle(), bytes(arena, term), term.length, output));
            return booleanOutput(output);
        }
    }

    private Lookup getText(byte[] term) {
        ensureTextDomain();
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment found = arena.allocate(JAVA_BYTE);
            MemorySegment value = arena.allocate(Native.OPTIONAL_U64);
            Native.check(Native.getText(handle(), bytes(arena, term), term.length, found, value));
            return lookup(found, value);
        }
    }

    private static Lookup lookup(MemorySegment found, MemorySegment value) {
        boolean present = booleanOutput(found);
        OptionalLong optional = value.get(JAVA_BYTE, Native.OPTIONAL_PRESENT) == 0
                ? OptionalLong.empty()
                : OptionalLong.of(value.get(JAVA_LONG, Native.OPTIONAL_VALUE));
        return new Lookup(present, optional);
    }

    private static boolean booleanOutput(MemorySegment output) {
        return output.get(JAVA_BYTE, 0) != 0;
    }

    private void ensureTextDomain() {
        if (domain == UnitDomain.U64) {
            throw new IllegalStateException("u64 dictionary requires long[] terms");
        }
        handle();
    }

    private void ensureDomain(UnitDomain expected) {
        if (domain != expected) throw new IllegalStateException("dictionary unit domain is " + domain);
        handle();
    }
}

package io.vinarytree.libdictenstein;

import static java.lang.foreign.MemoryLayout.PathElement.groupElement;
import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_BYTE;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemoryLayout;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.SymbolLookup;
import java.lang.invoke.MethodHandle;

/** Java FFM downcalls for the stable project ABI. */
final class Native {
    static final int OK = 0;
    static final MemoryLayout OPTIONAL_U64 = MemoryLayout.structLayout(
            JAVA_LONG.withName("value"),
            JAVA_BYTE.withName("has_value"),
            MemoryLayout.sequenceLayout(7, JAVA_BYTE).withName("reserved"));
    static final long OPTIONAL_VALUE = OPTIONAL_U64.byteOffset(groupElement("value"));
    static final long OPTIONAL_PRESENT = OPTIONAL_U64.byteOffset(groupElement("has_value"));
    static final MemoryLayout ENTRY = MemoryLayout.structLayout(
            ADDRESS.withName("data"),
            JAVA_LONG.withName("len"),
            OPTIONAL_U64.withName("value"));
    static final long ENTRY_DATA = ENTRY.byteOffset(groupElement("data"));
    static final long ENTRY_LEN = ENTRY.byteOffset(groupElement("len"));
    static final long ENTRY_VALUE = ENTRY.byteOffset(groupElement("value"));

    private static final Linker LINKER = Linker.nativeLinker();
    private static final MethodHandle LAST_ERROR;
    private static final MethodHandle NEW;
    private static final MethodHandle DAT_NEW;
    private static final MethodHandle SCDAWG_NEW;
    private static final MethodHandle PERSISTENT_CREATE;
    private static final MethodHandle PERSISTENT_OPEN;
    private static final MethodHandle VOCAB_CREATE;
    private static final MethodHandle VOCAB_OPEN;
    private static final MethodHandle FREE;
    private static final MethodHandle RESOURCE;
    private static final MethodHandle LEN;
    private static final MethodHandle CLEAR;
    private static final MethodHandle COMPACT;
    private static final MethodHandle INSERT_TEXT;
    private static final MethodHandle REMOVE_TEXT;
    private static final MethodHandle CONTAINS_TEXT;
    private static final MethodHandle GET_TEXT;
    private static final MethodHandle INSERT_U64;
    private static final MethodHandle REMOVE_U64;
    private static final MethodHandle CONTAINS_U64;
    private static final MethodHandle GET_U64;
    private static final MethodHandle INSERT_TEXT_BATCH;
    private static final MethodHandle INSERT_U64_BATCH;
    private static final MethodHandle KIND;
    private static final MethodHandle CAPABILITIES;
    private static final MethodHandle CHECKPOINT;
    private static final MethodHandle CONTAINS_SUBSTRING;
    private static final MethodHandle SUBSTRING_FREQUENCY;
    private static final MethodHandle VOCAB_TERM;

    static {
        NativeLibraryLoader.load();
        SymbolLookup symbols = SymbolLookup.loaderLookup();
        LAST_ERROR = downcall(symbols, "ldict_last_error_message", FunctionDescriptor.of(ADDRESS));
        NEW = downcall(symbols, "ldict_dynamic_dawg_new",
                FunctionDescriptor.of(JAVA_INT, JAVA_INT, ADDRESS));
        DAT_NEW = downcall(symbols, "ldict_double_array_trie_new",
                FunctionDescriptor.of(JAVA_INT, JAVA_INT, ADDRESS, JAVA_LONG, ADDRESS));
        SCDAWG_NEW = downcall(symbols, "ldict_scdawg_new",
                FunctionDescriptor.of(JAVA_INT, JAVA_INT, ADDRESS));
        PERSISTENT_CREATE = persistent(symbols, "ldict_persistent_artrie_create", true);
        PERSISTENT_OPEN = persistent(symbols, "ldict_persistent_artrie_open", true);
        VOCAB_CREATE = persistent(symbols, "ldict_persistent_vocab_create", false);
        VOCAB_OPEN = persistent(symbols, "ldict_persistent_vocab_open", false);
        FREE = downcall(symbols, "ldict_dictionary_free", FunctionDescriptor.ofVoid(ADDRESS));
        RESOURCE = downcall(symbols, "ldict_dictionary_resource",
                FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS));
        LEN = downcall(symbols, "ldict_dictionary_len",
                FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS));
        CLEAR = downcall(symbols, "ldict_dictionary_clear",
                FunctionDescriptor.of(JAVA_INT, ADDRESS));
        COMPACT = downcall(symbols, "ldict_dictionary_compact",
                FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS));
        INSERT_TEXT = mutation(symbols, "ldict_dictionary_insert_text", true);
        REMOVE_TEXT = mutation(symbols, "ldict_dictionary_remove_text", false);
        CONTAINS_TEXT = mutation(symbols, "ldict_dictionary_contains_text", false);
        GET_TEXT = getter(symbols, "ldict_dictionary_get_text");
        INSERT_U64 = mutation(symbols, "ldict_dictionary_insert_u64", true);
        REMOVE_U64 = mutation(symbols, "ldict_dictionary_remove_u64", false);
        CONTAINS_U64 = mutation(symbols, "ldict_dictionary_contains_u64", false);
        GET_U64 = getter(symbols, "ldict_dictionary_get_u64");
        INSERT_TEXT_BATCH = batch(symbols, "ldict_dictionary_insert_text_batch");
        INSERT_U64_BATCH = batch(symbols, "ldict_dictionary_insert_u64_batch");
        KIND = downcall(symbols, "ldict_dictionary_kind",
                FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS));
        CAPABILITIES = downcall(symbols, "ldict_dictionary_capabilities",
                FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS));
        CHECKPOINT = downcall(symbols, "ldict_dictionary_checkpoint",
                FunctionDescriptor.of(JAVA_INT, ADDRESS));
        CONTAINS_SUBSTRING = getterBoolean(symbols, "ldict_scdawg_contains_substring");
        SUBSTRING_FREQUENCY = downcall(symbols, "ldict_scdawg_substring_frequency",
                FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG, ADDRESS));
        VOCAB_TERM = downcall(symbols, "ldict_vocab_get_term",
                FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_LONG, ADDRESS, JAVA_LONG,
                        ADDRESS, ADDRESS));
    }

    private Native() {}

    private static MethodHandle mutation(SymbolLookup symbols, String name, boolean value) {
        return downcall(symbols, name, value
                ? FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG, OPTIONAL_U64, ADDRESS)
                : FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG, ADDRESS));
    }

    private static MethodHandle getter(SymbolLookup symbols, String name) {
        return downcall(symbols, name,
                FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG, ADDRESS, ADDRESS));
    }

    private static MethodHandle batch(SymbolLookup symbols, String name) {
        return downcall(symbols, name,
                FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG, ADDRESS));
    }

    private static MethodHandle persistent(SymbolLookup symbols, String name, boolean domain) {
        return downcall(symbols, name, domain
                ? FunctionDescriptor.of(JAVA_INT, JAVA_INT, ADDRESS, JAVA_LONG, ADDRESS)
                : FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_LONG, ADDRESS));
    }

    private static MethodHandle getterBoolean(SymbolLookup symbols, String name) {
        return downcall(symbols, name,
                FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_LONG, ADDRESS));
    }

    private static MethodHandle downcall(
            SymbolLookup symbols, String name, FunctionDescriptor descriptor) {
        return LINKER.downcallHandle(symbols.find(name).orElseThrow(), descriptor);
    }

    static void check(int status) {
        if (status != OK) throw new NativeException(status, lastError());
    }

    static String lastError() {
        MemorySegment address = (MemorySegment) invoke(LAST_ERROR);
        return address.equals(MemorySegment.NULL)
                ? "native operation failed"
                : address.reinterpret(4096).getString(0);
    }

    static int create(int domain, MemorySegment out) { return call(NEW, domain, out); }
    static int createDat(int domain, MemorySegment entries, long len, MemorySegment out) {
        return call(DAT_NEW, domain, entries, len, out);
    }
    static int createScdawg(int domain, MemorySegment out) { return call(SCDAWG_NEW, domain, out); }
    static int persistentCreate(int domain, MemorySegment path, long len, MemorySegment out) {
        return call(PERSISTENT_CREATE, domain, path, len, out);
    }
    static int persistentOpen(int domain, MemorySegment path, long len, MemorySegment out) {
        return call(PERSISTENT_OPEN, domain, path, len, out);
    }
    static int vocabCreate(MemorySegment path, long len, MemorySegment out) {
        return call(VOCAB_CREATE, path, len, out);
    }
    static int vocabOpen(MemorySegment path, long len, MemorySegment out) {
        return call(VOCAB_OPEN, path, len, out);
    }
    static void free(MemorySegment handle) { invoke(FREE, handle); }
    static int resource(MemorySegment handle, MemorySegment out) { return call(RESOURCE, handle, out); }
    static int len(MemorySegment handle, MemorySegment out) { return call(LEN, handle, out); }
    static int clear(MemorySegment handle) { return call(CLEAR, handle); }
    static int compact(MemorySegment handle, MemorySegment out) { return call(COMPACT, handle, out); }
    static int insertText(MemorySegment handle, MemorySegment data, long len,
                          MemorySegment value, MemorySegment out) {
        return call(INSERT_TEXT, handle, data, len, value, out);
    }
    static int removeText(MemorySegment handle, MemorySegment data, long len, MemorySegment out) {
        return call(REMOVE_TEXT, handle, data, len, out);
    }
    static int containsText(MemorySegment handle, MemorySegment data, long len, MemorySegment out) {
        return call(CONTAINS_TEXT, handle, data, len, out);
    }
    static int getText(MemorySegment handle, MemorySegment data, long len,
                       MemorySegment found, MemorySegment value) {
        return call(GET_TEXT, handle, data, len, found, value);
    }
    static int insertU64(MemorySegment handle, MemorySegment data, long len,
                         MemorySegment value, MemorySegment out) {
        return call(INSERT_U64, handle, data, len, value, out);
    }
    static int removeU64(MemorySegment handle, MemorySegment data, long len, MemorySegment out) {
        return call(REMOVE_U64, handle, data, len, out);
    }
    static int containsU64(MemorySegment handle, MemorySegment data, long len, MemorySegment out) {
        return call(CONTAINS_U64, handle, data, len, out);
    }
    static int getU64(MemorySegment handle, MemorySegment data, long len,
                      MemorySegment found, MemorySegment value) {
        return call(GET_U64, handle, data, len, found, value);
    }
    static int insertTextBatch(MemorySegment handle, MemorySegment entries, long len,
                               MemorySegment out) {
        return call(INSERT_TEXT_BATCH, handle, entries, len, out);
    }
    static int insertU64Batch(MemorySegment handle, MemorySegment entries, long len,
                              MemorySegment out) {
        return call(INSERT_U64_BATCH, handle, entries, len, out);
    }
    static int kind(MemorySegment handle, MemorySegment out) { return call(KIND, handle, out); }
    static int capabilities(MemorySegment handle, MemorySegment out) {
        return call(CAPABILITIES, handle, out);
    }
    static int checkpoint(MemorySegment handle) { return call(CHECKPOINT, handle); }
    static int containsSubstring(MemorySegment handle, MemorySegment data, long len,
                                 MemorySegment out) {
        return call(CONTAINS_SUBSTRING, handle, data, len, out);
    }
    static int substringFrequency(MemorySegment handle, MemorySegment data, long len,
                                  MemorySegment out) {
        return call(SUBSTRING_FREQUENCY, handle, data, len, out);
    }
    static int vocabTerm(MemorySegment handle, long index, MemorySegment data, long capacity,
                         MemorySegment length, MemorySegment found) {
        return call(VOCAB_TERM, handle, index, data, capacity, length, found);
    }

    private static int call(MethodHandle handle, Object... arguments) {
        return (int) invoke(handle, arguments);
    }

    private static Object invoke(MethodHandle handle, Object... arguments) {
        try {
            return handle.invokeWithArguments(arguments);
        } catch (Throwable throwable) {
            if (throwable instanceof RuntimeException runtime) throw runtime;
            throw new RuntimeException(throwable);
        }
    }
}

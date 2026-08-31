package io.vinarytree.libdictenstein;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.OptionalLong;
import java.util.Random;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.atomic.AtomicBoolean;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/**
 * Uniform facade conformance suite for the JVM binding.
 *
 * <p>Instantiates the family C1-C10 contract for the JVM against a live
 * libdictenstein shared library. Unlike {@code CrossProjectSnapshotTest} /
 * {@code BackendIntegrationTest} this suite needs only libdictenstein and the
 * canonical fixture, never a liblevenshtein transducer, so it pins the
 * <em>producer</em> ABI in isolation.
 *
 * <ul>
 *   <li>C1 identity + kind/capabilities per backend
 *   <li>C2 idempotent close + free-order independence
 *   <li>C3 reachable status arms (INVALID_UTF8, DOMAIN_MISMATCH, IO_ERROR) + message
 *       (NULL_POINTER/UNSUPPORTED/LIMIT_EXCEEDED marked N/A with a reason)
 *   <li>C4 canonical fixture replay (all four backends)
 *   <li>C5 CRUD + value + batch + substring; capability-derived rejects
 *   <li>C6 precomposed/combining/multibyte, byte-domain NUL + invalid UTF-8, u64 0/MAX
 *   <li>C7 batch sizes 0/1/255/256/257/1000
 *   <li>C8 CRUD op-script vs a Map oracle; substring vs a naive oracle
 *   <li>C9 12k create/use/free cycles with /proc VmRSS bounded
 *   <li>C10 independent per-thread dictionaries + concurrent readers during a writer
 * </ul>
 */
final class ConformanceTest {

    // Capability bits (LDICT_CAP_*).
    private static final long READ = 1L << 0, INSERT = 1L << 1, REMOVE = 1L << 2,
            CLEAR = 1L << 3, COMPACT = 1L << 4, SUBSTRING = 1L << 5, CHECKPOINT = 1L << 6;

    private static final Map<String, Object> FIXTURE = loadFixture();

    // -----------------------------------------------------------------------
    // fixture (C4) helpers
    // -----------------------------------------------------------------------

    @SuppressWarnings("unchecked")
    private static Map<String, Object> loadFixture() {
        List<String> candidates = List.of(
                System.getProperty("vinaryTree.fixture", ""),
                "../canonical_fixture.json",
                "bindings/canonical_fixture.json",
                "../../bindings/canonical_fixture.json");
        for (String candidate : candidates) {
            if (candidate.isEmpty()) continue;
            Path path = Path.of(candidate);
            if (Files.isRegularFile(path)) {
                try {
                    return (Map<String, Object>) Json.parse(Files.readString(path, StandardCharsets.UTF_8));
                } catch (IOException error) {
                    throw new RuntimeException(error);
                }
            }
        }
        throw new IllegalStateException("canonical_fixture.json not found (set -DvinaryTree.fixture)");
    }

    @SuppressWarnings("unchecked")
    private static List<Map<String, Object>> array(String key) {
        return (List<Map<String, Object>>) (List<?>) FIXTURE.get(key);
    }

    private static OptionalLong optional(Object value) {
        return value == null ? OptionalLong.empty() : OptionalLong.of((Long) value);
    }

    private static Map<String, OptionalLong> fixtureEntries() {
        Map<String, OptionalLong> entries = new LinkedHashMap<>();
        for (Map<String, Object> item : array("entries"))
            entries.put((String) item.get("term"), optional(item.get("value")));
        return entries;
    }

    // -----------------------------------------------------------------------
    // C1 identity/version
    // -----------------------------------------------------------------------

    @Test
    void c1_identityConstants() {
        assertEquals(1, Dictionary.abiVersion());
        assertEquals(4, Dictionary.apiRevision());
    }

    @Test
    void c1_kindAndCapabilities() {
        try (var dawg = new DynamicDawg()) {
            assertEquals(1, dawg.kind());
            long caps = dawg.capabilities();
            assertTrue((caps & INSERT) != 0 && (caps & REMOVE) != 0
                    && (caps & CLEAR) != 0 && (caps & COMPACT) != 0);
            assertTrue((caps & SUBSTRING) == 0 && (caps & CHECKPOINT) == 0);
        }
        try (var dat = new DoubleArrayTrie(Map.of("x", OptionalLong.empty()))) {
            assertEquals(2, dat.kind());
            assertTrue((dat.capabilities() & READ) != 0);
            assertTrue((dat.capabilities() & INSERT) == 0);
        }
        try (var scdawg = new Scdawg()) {
            assertEquals(3, scdawg.kind());
            assertTrue((scdawg.capabilities() & SUBSTRING) != 0);
        }
    }

    // -----------------------------------------------------------------------
    // C2 lifecycle/ownership
    // -----------------------------------------------------------------------

    @Test
    void c2_doubleCloseIsIdempotent() {
        var dawg = new DynamicDawg();
        dawg.put("a", OptionalLong.empty());
        dawg.close();
        dawg.close(); // no double free, no crash
    }

    @Test
    void c2_freeOrderIndependence() {
        List<DynamicDawg> dicts = new ArrayList<>();
        for (int i = 0; i < 4; i++) {
            var dawg = new DynamicDawg();
            dawg.put("term" + i, OptionalLong.of(i));
            dicts.add(dawg);
        }
        for (int index : new int[] {2, 0, 3, 1}) dicts.get(index).close();
    }

    // -----------------------------------------------------------------------
    // C3 error-mapping matrix + thread-local message
    //
    // Reachable through the idiomatic typed API: INVALID_UTF8 (3), IO_ERROR (7).
    // N/A:
    //   - NULL_POINTER (4):    a closed handle throws IllegalStateException
    //                          before crossing the ABI.
    //   - DOMAIN_MISMATCH (9): the facade guards unit-domain misuse with an
    //                          IllegalStateException before crossing the ABI
    //                          (asserted below); the native arm is unreachable.
    //   - UNSUPPORTED (6):     no typed method exposes an unadvertised
    //                          operation; capability bits are asserted absent (C5).
    //   - LIMIT_EXCEEDED (10): PersistentVocabulary#term auto-sizes its buffer.
    // -----------------------------------------------------------------------

    @Test
    void c3_invalidUtf8() {
        try (var dawg = new DynamicDawg(UnitDomain.UNICODE_SCALAR)) {
            var error = assertThrows(NativeException.class,
                    () -> dawg.put(new byte[] {(byte) 0xFF}, OptionalLong.empty()));
            assertEquals(3, error.status());
            assertNotNull(error.getMessage());
            assertFalse(error.getMessage().isEmpty());
        }
    }

    @Test
    void c3_crossDomainGuardedByFacade() {
        // The JVM facade rejects unit-domain misuse client-side, so the native
        // DOMAIN_MISMATCH (9) arm is never reached through the typed API.
        try (var dawg = new DynamicDawg(UnitDomain.UNICODE_SCALAR)) {
            assertThrows(IllegalStateException.class,
                    () -> dawg.put(new long[] {1, 2}, OptionalLong.empty()));
        }
    }

    @Test
    void c3_ioErrorOnMissingPersistent(@TempDir Path directory) {
        var error = assertThrows(NativeException.class,
                () -> PersistentARTrie.open(directory.resolve("does-not-exist.part")));
        assertEquals(7, error.status());
        assertNotNull(error.getMessage());
        assertFalse(error.getMessage().isEmpty());
    }

    // -----------------------------------------------------------------------
    // C4 canonical fixture replay (cross-language oracle)
    // -----------------------------------------------------------------------

    private void assertFixtureReads(Dictionary dictionary) {
        assertEquals(((Long) FIXTURE.get("size")).longValue(), dictionary.size());
        for (Map<String, Object> item : array("contains"))
            assertEquals(item.get("expected"), dictionary.contains((String) item.get("term")),
                    (String) item.get("term"));
        for (Map<String, Object> item : array("get")) {
            var lookup = dictionary.get((String) item.get("term"));
            assertEquals(item.get("found"), lookup.present(), (String) item.get("term"));
            assertEquals(optional(item.get("value")), lookup.value(), (String) item.get("term"));
        }
    }

    @Test
    void c4_dynamicDawgMatchesOracle() {
        try (var dawg = new DynamicDawg()) {
            assertEquals(((Long) FIXTURE.get("size")).longValue(), dawg.putAllStrings(fixtureEntries()));
            assertFixtureReads(dawg);
        }
    }

    @Test
    void c4_doubleArrayTrieMatchesOracle() {
        try (var dat = new DoubleArrayTrie(fixtureEntries())) {
            assertFixtureReads(dat);
        }
    }

    @Test
    void c4_persistentArtrieMatchesOracle(@TempDir Path directory) {
        try (var art = PersistentARTrie.create(directory.resolve("terms.part"))) {
            assertEquals(((Long) FIXTURE.get("size")).longValue(), art.putAllStrings(fixtureEntries()));
            assertFixtureReads(art);
        }
    }

    @Test
    void c4_scdawgMatchesSubstringOracle() {
        try (var scdawg = new Scdawg()) {
            scdawg.putAllStrings(fixtureEntries());
            for (Map<String, Object> item : array("substring_frequency"))
                assertEquals(((Long) item.get("expected")).longValue(),
                        scdawg.frequency((String) item.get("pattern")), (String) item.get("pattern"));
            for (Map<String, Object> item : array("substring_contains"))
                assertEquals(item.get("expected"),
                        scdawg.containsSubstring((String) item.get("pattern")),
                        (String) item.get("pattern"));
        }
    }

    // -----------------------------------------------------------------------
    // C5 CRUD + value + batch + substring; capability-derived rejects
    // -----------------------------------------------------------------------

    @Test
    void c5_crudRoundTrip() {
        try (var dawg = new DynamicDawg()) {
            assertTrue(dawg.put("cat", OptionalLong.of(1)));
            assertFalse(dawg.put("cat", OptionalLong.of(1))); // idempotent
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(1)), dawg.get("cat"));
            assertTrue(dawg.remove("cat"));
            assertFalse(dawg.remove("cat"));
            assertFalse(dawg.contains("cat"));
        }
    }

    @Test
    void c5_compactPreservesTerms() {
        try (var dawg = new DynamicDawg()) {
            Map<String, OptionalLong> batch = new LinkedHashMap<>();
            for (int i = 0; i < 50; i++) batch.put("t" + i, OptionalLong.of(i));
            dawg.putAllStrings(batch);
            for (int i = 0; i < 50; i += 2) assertTrue(dawg.remove("t" + i));
            dawg.compact();
            assertEquals(25, dawg.size());
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(1)), dawg.get("t1"));
            assertFalse(dawg.contains("t0"));
        }
    }

    @Test
    void c5_substringUpdatesWithInserts() {
        try (var scdawg = new Scdawg()) {
            scdawg.put("cat", OptionalLong.of(1));
            scdawg.put("cot", OptionalLong.of(2));
            assertEquals(2, scdawg.frequency("t"));
            assertTrue(scdawg.put("cut", OptionalLong.empty()));
            assertEquals(3, scdawg.frequency("t"));
        }
    }

    @Test
    void c5_capabilityDerivedRejects() {
        try (var dat = new DoubleArrayTrie(Map.of("x", OptionalLong.empty()))) {
            assertTrue((dat.capabilities() & (INSERT | REMOVE | CLEAR | COMPACT)) == 0);
        }
        // Cross-domain use is rejected client-side by the facade domain guard.
        try (var dawg = new DynamicDawg(UnitDomain.UNICODE_SCALAR)) {
            assertThrows(IllegalStateException.class,
                    () -> dawg.put(new long[] {1}, OptionalLong.empty()));
        }
    }

    // -----------------------------------------------------------------------
    // C6 text domains and values
    // -----------------------------------------------------------------------

    @Test
    void c6_precomposedAndMultibyte() {
        try (var dawg = new DynamicDawg()) {
            assertTrue(dawg.put("café", OptionalLong.of(7))); // precomposed U+00E9
            assertTrue(dawg.put("🦀", OptionalLong.of(255))); // 🦀, 4-byte scalar
            assertTrue(dawg.contains("café"));
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(255)), dawg.get("🦀"));
        }
    }

    @Test
    void c6_combiningDistinctFromPrecomposed() {
        try (var dawg = new DynamicDawg()) {
            assertTrue(dawg.put("café", OptionalLong.of(1)));  // precomposed U+00E9
            assertTrue(dawg.put("café", OptionalLong.of(2))); // cafe + U+0301
            assertEquals(2, dawg.size());
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(1)), dawg.get("café"));
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(2)), dawg.get("café"));
        }
    }

    @Test
    void c6_byteDomainAcceptsNulAndInvalidUtf8() {
        try (var dawg = new DynamicDawg(UnitDomain.BYTE)) {
            byte[] embeddedNul = {'a', 0x00, 'b'};
            byte[] invalidUtf8 = {(byte) 0xFF, (byte) 0xFE};
            assertTrue(dawg.put(embeddedNul, OptionalLong.of(1)));
            assertTrue(dawg.put(invalidUtf8, OptionalLong.of(2)));
            assertTrue(dawg.contains(embeddedNul));
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(2)), dawg.get(invalidUtf8));
        }
    }

    @Test
    void c6_u64DomainValuesZeroAndMax() {
        try (var dawg = new DynamicDawg(UnitDomain.U64)) {
            assertTrue(dawg.put(new long[] {1, 2, 3}, OptionalLong.of(0)));
            assertTrue(dawg.put(new long[] {9}, OptionalLong.of(-1L))); // u64 MAX
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(0)), dawg.get(new long[] {1, 2, 3}));
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(-1L)), dawg.get(new long[] {9}));
        }
    }

    // -----------------------------------------------------------------------
    // C7 batch / paging edges
    // -----------------------------------------------------------------------

    @Test
    void c7_batchSizes() {
        for (int size : new int[] {0, 1, 255, 256, 257, 1000}) {
            try (var dawg = new DynamicDawg()) {
                Map<String, OptionalLong> batch = new LinkedHashMap<>();
                for (int i = 0; i < size; i++) batch.put("t" + i, OptionalLong.of(i));
                assertEquals(size, dawg.putAllStrings(batch));
                assertEquals(size, dawg.size());
                if (size > 0) {
                    assertEquals(new Dictionary.Lookup(true, OptionalLong.of(0)), dawg.get("t0"));
                    assertEquals(new Dictionary.Lookup(true, OptionalLong.of(size - 1)),
                            dawg.get("t" + (size - 1)));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // C8 property-based testing vs an in-language oracle
    // -----------------------------------------------------------------------

    @Test
    void c8_crudScriptMatchesMapOracle() {
        Random rng = new Random(0xC0FFEEL);
        String[] keys = new String[40];
        for (int i = 0; i < keys.length; i++) keys[i] = "k" + i;
        Map<String, OptionalLong> oracle = new LinkedHashMap<>();
        try (var dawg = new DynamicDawg()) {
            for (int step = 0; step < 3000; step++) {
                String key = keys[rng.nextInt(keys.length)];
                boolean present = oracle.containsKey(key);
                double op = rng.nextDouble();
                if (op < 0.5) {
                    OptionalLong value = rng.nextBoolean()
                            ? OptionalLong.of(rng.nextInt(1 << 30)) : OptionalLong.empty();
                    assertEquals(!present, dawg.put(key, value));
                    oracle.put(key, value);
                } else if (op < 0.75) {
                    assertEquals(present, dawg.remove(key));
                    oracle.remove(key);
                } else if (op < 0.95) {
                    assertEquals(present, dawg.contains(key));
                    if (present) assertEquals(new Dictionary.Lookup(true, oracle.get(key)), dawg.get(key));
                } else {
                    dawg.compact();
                }
                assertEquals(oracle.size(), dawg.size());
            }
        }
    }

    @Test
    void c8_substringMatchesNaiveOracle() {
        Random rng = new Random(0x5CDAL);
        char[] alphabet = {'a', 'b', 'c', 'x'};
        java.util.Set<String> terms = new java.util.LinkedHashSet<>();
        while (terms.size() < 60) {
            int n = 1 + rng.nextInt(6);
            StringBuilder builder = new StringBuilder();
            for (int i = 0; i < n; i++) builder.append(alphabet[rng.nextInt(alphabet.length)]);
            terms.add(builder.toString());
        }
        try (var scdawg = new Scdawg()) {
            for (String term : terms) scdawg.put(term, OptionalLong.empty());
            for (int i = 0; i < 200; i++) {
                int n = 1 + rng.nextInt(3);
                StringBuilder builder = new StringBuilder();
                for (int j = 0; j < n; j++) builder.append(alphabet[rng.nextInt(alphabet.length)]);
                String pattern = builder.toString();
                long expected = 0;
                for (String term : terms)
                    for (int start = 0; start + pattern.length() <= term.length(); start++)
                        if (term.regionMatches(start, pattern, 0, pattern.length())) expected++;
                assertEquals(expected, scdawg.frequency(pattern), pattern);
                assertEquals(expected > 0, scdawg.containsSubstring(pattern), pattern);
            }
        }
    }

    // -----------------------------------------------------------------------
    // C9 leak discipline
    // -----------------------------------------------------------------------

    private static long rssKib() {
        try {
            for (String line : Files.readAllLines(Path.of("/proc/self/status"))) {
                if (line.startsWith("VmRSS:")) return Long.parseLong(line.replaceAll("\\D+", ""));
            }
        } catch (IOException | NumberFormatException ignored) {
            // not available; the leak test degrades to a no-op
        }
        return 0;
    }

    @Test
    void c9_createUseFreeCyclesDoNotLeak() {
        final int cycles = 12000;
        Map<String, OptionalLong> batch = new LinkedHashMap<>();
        batch.put("cat", OptionalLong.of(1));
        batch.put("cot", OptionalLong.of(2));
        batch.put("cut", OptionalLong.empty());
        for (int warmup = 0; warmup < 2000; warmup++) {
            try (var dawg = new DynamicDawg()) {
                dawg.put("cat", OptionalLong.of(1));
            }
        }
        System.gc();
        long before = rssKib();
        for (int i = 0; i < cycles; i++) {
            try (var dawg = new DynamicDawg()) {
                dawg.putAllStrings(batch);
                assertTrue(dawg.contains("cot"));
            }
        }
        System.gc();
        long after = rssKib();
        // JVM RSS is noisy; a per-cycle native leak would blow far past this
        // headroom over 12k cycles.
        if (before != 0 && after > before) {
            assertTrue(after - before < 96L * 1024L,
                    "RSS grew " + (after - before) + " KiB over " + cycles + " cycles");
        }
    }

    // -----------------------------------------------------------------------
    // C10 concurrency
    // -----------------------------------------------------------------------

    @Test
    void c10_independentDictionariesPerThread() throws InterruptedException {
        List<Throwable> errors = new CopyOnWriteArrayList<>();
        List<Thread> threads = new ArrayList<>();
        for (int seed = 0; seed < 8; seed++) {
            final int s = seed;
            Thread thread = new Thread(() -> {
                try (var dawg = new DynamicDawg()) {
                    for (int i = 0; i < 2000; i++) dawg.put("t" + s + "_" + i, OptionalLong.of(i));
                    assertEquals(2000, dawg.size());
                    assertEquals(new Dictionary.Lookup(true, OptionalLong.of(1500)),
                            dawg.get("t" + s + "_1500"));
                } catch (Throwable failure) {
                    errors.add(failure);
                }
            });
            threads.add(thread);
            thread.start();
        }
        for (Thread thread : threads) thread.join();
        assertTrue(errors.isEmpty(), () -> errors.toString());
    }

    @Test
    void c10_concurrentReadersDuringWriter() throws InterruptedException {
        List<Throwable> errors = new CopyOnWriteArrayList<>();
        try (var dawg = new DynamicDawg()) {
            Map<String, OptionalLong> seed = new LinkedHashMap<>();
            for (int i = 0; i < 500; i++) seed.put("seed" + i, OptionalLong.of(i));
            dawg.putAllStrings(seed);
            AtomicBoolean stop = new AtomicBoolean(false);
            List<Thread> readers = new ArrayList<>();
            for (int r = 0; r < 4; r++) {
                Thread reader = new Thread(() -> {
                    try {
                        while (!stop.get()) {
                            assertTrue(dawg.contains("seed0"));
                            dawg.get("seed250");
                        }
                    } catch (Throwable failure) {
                        errors.add(failure);
                    }
                });
                readers.add(reader);
                reader.start();
            }
            for (int i = 500; i < 3000; i++) dawg.put("w" + i, OptionalLong.of(i));
            stop.set(true);
            for (Thread reader : readers) reader.join();
            assertTrue(errors.isEmpty(), () -> errors.toString());
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(2999)), dawg.get("w2999"));
        }
    }

    // -----------------------------------------------------------------------
    // minimal JSON parser (Map / List / String / Long / Boolean / null)
    // -----------------------------------------------------------------------

    @Test
    void jsonParserUsesAStackSafeContainerMachine() {
        int depth = 100_000;
        Object value = Json.parse("[".repeat(depth) + "0" + "]".repeat(depth));
        for (int level = 0; level < depth; level++) {
            List<?> array = (List<?>) value;
            assertEquals(1, array.size());
            value = array.get(0);
        }
        assertEquals(0L, value);
        assertThrows(IllegalArgumentException.class, () -> Json.parse("[] trailing"));
        assertThrows(IllegalArgumentException.class, () -> Json.parse("{\"missing\":}"));
    }

    private static final class Json {
        private static final class ContainerFrame {
            private final Map<String, Object> object;
            private final List<Object> array;
            private String pendingKey;

            private ContainerFrame(Map<String, Object> object, List<Object> array) {
                this.object = object;
                this.array = array;
            }

            static ContainerFrame object(Map<String, Object> object, String pendingKey) {
                ContainerFrame frame = new ContainerFrame(object, null);
                frame.pendingKey = pendingKey;
                return frame;
            }

            static ContainerFrame array(List<Object> array) {
                return new ContainerFrame(null, array);
            }

            boolean isObject() {
                return object != null;
            }

            void accept(Object value) {
                if (isObject()) {
                    object.put(pendingKey, value);
                    pendingKey = null;
                } else {
                    array.add(value);
                }
            }

            Object completedValue() {
                return isObject() ? object : array;
            }
        }

        private final String s;
        private int i;

        private Json(String text) { this.s = text; }

        static Object parse(String text) {
            Json parser = new Json(text);
            Object value = parser.value();
            parser.ws();
            if (parser.i != parser.s.length()) {
                throw parser.error("trailing input");
            }
            return value;
        }

        private Object value() {
            ArrayDeque<ContainerFrame> frames = new ArrayDeque<>();
            Object completed = null;
            boolean hasCompletedValue = false;

            for (;;) {
                if (!hasCompletedValue) {
                    ws();
                    if (i == s.length()) {
                        throw error("expected a value");
                    }

                    char c = s.charAt(i);
                    switch (c) {
                        case '{' -> {
                            i++;
                            Map<String, Object> object = new LinkedHashMap<>();
                            ws();
                            if (consume('}')) {
                                completed = object;
                                hasCompletedValue = true;
                            } else {
                                String key = string();
                                ws();
                                expect(':');
                                frames.push(ContainerFrame.object(object, key));
                                continue;
                            }
                        }
                        case '[' -> {
                            i++;
                            List<Object> array = new ArrayList<>();
                            ws();
                            if (consume(']')) {
                                completed = array;
                                hasCompletedValue = true;
                            } else {
                                frames.push(ContainerFrame.array(array));
                                continue;
                            }
                        }
                        case '"' -> {
                            completed = string();
                            hasCompletedValue = true;
                        }
                        case 't' -> {
                            literal("true");
                            completed = Boolean.TRUE;
                            hasCompletedValue = true;
                        }
                        case 'f' -> {
                            literal("false");
                            completed = Boolean.FALSE;
                            hasCompletedValue = true;
                        }
                        case 'n' -> {
                            literal("null");
                            completed = null;
                            hasCompletedValue = true;
                        }
                        default -> {
                            completed = number();
                            hasCompletedValue = true;
                        }
                    }
                }

                if (frames.isEmpty()) {
                    return completed;
                }

                ContainerFrame parent = frames.peek();
                parent.accept(completed);
                ws();
                if (parent.isObject()) {
                    if (consume(',')) {
                        ws();
                        parent.pendingKey = string();
                        ws();
                        expect(':');
                        hasCompletedValue = false;
                        continue;
                    }
                    expect('}');
                } else {
                    if (consume(',')) {
                        hasCompletedValue = false;
                        continue;
                    }
                    expect(']');
                }

                frames.pop();
                completed = parent.completedValue();
                hasCompletedValue = true;
            }
        }

        private String string() {
            ws();
            expect('"');
            StringBuilder builder = new StringBuilder();
            while (i < s.length() && s.charAt(i) != '"') {
                char c = s.charAt(i++);
                if (c != '\\') { builder.append(c); continue; }
                if (i == s.length()) throw error("unterminated escape");
                char e = s.charAt(i++);
                switch (e) {
                    case 'n' -> builder.append('\n');
                    case 't' -> builder.append('\t');
                    case 'r' -> builder.append('\r');
                    case 'u' -> {
                        if (i + 4 > s.length()) throw error("truncated Unicode escape");
                        builder.append((char) Integer.parseInt(s.substring(i, i + 4), 16));
                        i += 4;
                    }
                    default -> builder.append(e);
                }
            }
            expect('"');
            return builder.toString();
        }

        private Object number() {
            int start = i;
            while (i < s.length() && "+-.eE0123456789".indexOf(s.charAt(i)) >= 0) i++;
            if (start == i) throw error("expected a number");
            return Long.valueOf(Long.parseLong(s.substring(start, i)));
        }

        private void literal(String literal) {
            if (!s.startsWith(literal, i)) throw error("expected " + literal);
            i += literal.length();
        }

        private boolean consume(char expected) {
            if (i < s.length() && s.charAt(i) == expected) {
                i++;
                return true;
            }
            return false;
        }

        private void expect(char expected) {
            if (!consume(expected)) throw error("expected '" + expected + "'");
        }

        private IllegalArgumentException error(String message) {
            return new IllegalArgumentException(message + " at byte " + i);
        }

        private void ws() {
            while (i < s.length() && Character.isWhitespace(s.charAt(i))) i++;
        }
    }
}

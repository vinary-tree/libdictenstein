package io.vinarytree.libdictenstein;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.List;
import java.util.OptionalLong;
import io.vinarytree.interop.DictionaryEntry;
import io.vinarytree.interop.DictionaryEntryIterator;
import io.vinarytree.interop.DictionarySnapshot;
import io.vinarytree.interop.DictionaryUnitDomain;

/** Deterministic benchmark entrypoint over the public JVM collection facade. */
public final class CollectionTraversalProfile {
    private static final String SCHEMA = "libdictenstein.host-collection-traversal.v1";
    private static final int KEY_UNITS = 38;

    /** One schema-compatible machine-readable result. */
    public record Result(
            String runtime,
            String arm,
            int dictionaryEntries,
            int consumedEntriesPerPass,
            int passes,
            int warmupPasses,
            Integer batchSize,
            Integer earlyCancel,
            long elapsedNs,
            long checksum) {
        /** Encode without a JSON dependency; checksum is emitted as an unsigned JSON integer. */
        public String toJson() {
            return "{\"schema\":\"" + SCHEMA
                    + "\",\"runtime\":\"" + runtime
                    + "\",\"arm\":\"" + arm
                    + "\",\"dictionary_entries\":" + dictionaryEntries
                    + ",\"consumed_entries_per_pass\":" + consumedEntriesPerPass
                    + ",\"passes\":" + passes
                    + ",\"warmup_passes\":" + warmupPasses
                    + ",\"batch_size\":" + (batchSize == null ? "null" : batchSize)
                    + ",\"early_cancel\":" + (earlyCancel == null ? "null" : earlyCancel)
                    + ",\"elapsed_ns\":" + elapsedNs
                    + ",\"checksum\":" + Long.toUnsignedString(checksum) + "}";
        }
    }

    private record Config(
            String arm, int entries, int passes, int warmupPasses, int batchSize, int earlyCancel) {
        int consumed() { return arm.equals("stream-cancel") ? Math.min(entries, earlyCancel) : entries; }
    }
    private record CorpusEntry(byte[] key, long value) {}
    private record Drain(long checksum, int count) {}

    private CollectionTraversalProfile() {}

    /** Run one arm. Construction and warmup are excluded from {@link Result#elapsedNs()}. */
    public static Result run(String... arguments) {
        Config config = parse(arguments);
        List<CorpusEntry> corpus = corpus(config.entries);
        long expected = expectedChecksum(corpus, config.consumed());
        try (DynamicDawg dictionary = new DynamicDawg(UnitDomain.BYTE)) {
            List<Dictionary.TextEntry> mutations = corpus.stream()
                    .map(entry -> new Dictionary.TextEntry(entry.key, OptionalLong.of(entry.value)))
                    .toList();
            long inserted = dictionary.putAllBytes(mutations);
            if (inserted != corpus.size()) {
                throw new IllegalStateException("generated corpus was not inserted exactly once");
            }
            for (int pass = 0; pass < config.warmupPasses; pass++) {
                verify(drain(dictionary, config), config.consumed(), expected, "warmup");
            }
            long started = System.nanoTime();
            long checksum = 0;
            for (int pass = 0; pass < config.passes; pass++) {
                Drain result = drain(dictionary, config);
                verify(result, config.consumed(), expected, "timed drain");
                checksum += result.checksum;
            }
            long elapsed = Math.max(1L, System.nanoTime() - started);
            if (checksum != expected * config.passes) {
                throw new IllegalStateException("aggregate checksum mismatch");
            }
            return new Result(
                    "jvm-java", config.arm, config.entries, config.consumed(), config.passes,
                    config.warmupPasses, config.arm.equals("materialized") ? null : config.batchSize,
                    config.arm.equals("stream-cancel") ? config.earlyCancel : null,
                    elapsed, checksum);
        }
    }

    /** CLI entrypoint. */
    public static void main(String[] arguments) {
        try {
            System.out.println(run(arguments).toJson());
        } catch (RuntimeException failure) {
            System.err.println(failure.getMessage());
            System.exit(2);
        }
    }

    private static Config parse(String[] arguments) {
        String arm = null;
        int entries = 65_536, passes = 1, warmup = 1, batch = 256, early = 64;
        if ((arguments.length & 1) != 0) throw new IllegalArgumentException("every option requires a value");
        for (int index = 0; index < arguments.length; index += 2) {
            String option = arguments[index], value = arguments[index + 1];
            switch (option) {
                case "--arm" -> arm = value;
                case "--entries" -> entries = positive(value, option, false);
                case "--passes" -> passes = positive(value, option, false);
                case "--warmup-passes" -> warmup = positive(value, option, true);
                case "--batch-size" -> batch = positive(value, option, false);
                case "--early-cancel" -> early = positive(value, option, false);
                default -> throw new IllegalArgumentException("unknown argument: " + option);
            }
        }
        if (!List.of("materialized", "stream", "stream-cancel").contains(arm)) {
            throw new IllegalArgumentException("--arm must be materialized, stream, or stream-cancel");
        }
        Math.multiplyExact(batch, KEY_UNITS);
        return new Config(arm, entries, passes, warmup, batch, early);
    }

    private static int positive(String value, String option, boolean allowZero) {
        int result;
        try { result = Integer.parseInt(value); }
        catch (NumberFormatException failure) { throw new IllegalArgumentException(option + " must be an integer"); }
        if (allowZero ? result < 0 : result <= 0) {
            throw new IllegalArgumentException(option + " must be " + (allowZero ? "nonnegative" : "positive"));
        }
        return result;
    }

    private static List<CorpusEntry> corpus(int size) {
        List<CorpusEntry> result = new ArrayList<>(size);
        for (int index = 0; index < size; index++) {
            String key = String.format("collection/%04x/%08x/shared-suffix", index & 0x0fff, index);
            result.add(new CorpusEntry(key.getBytes(StandardCharsets.US_ASCII), index));
        }
        return result;
    }

    private static long expectedChecksum(List<CorpusEntry> corpus, int limit) {
        List<CorpusEntry> ordered = new ArrayList<>(corpus);
        ordered.sort(Comparator.comparing(CorpusEntry::key, Arrays::compareUnsigned));
        long result = 0;
        for (int index = 0; index < limit; index++) {
            CorpusEntry entry = ordered.get(index);
            result += entry.key.length ^ entry.value;
        }
        return result;
    }

    private static Drain drain(DynamicDawg dictionary, Config config) {
        if (config.arm.equals("materialized")) {
            DictionarySnapshot snapshot = dictionary.snapshot();
            long checksum = 0;
            for (DictionaryEntry entry : snapshot) checksum += checksum(entry);
            return new Drain(checksum, snapshot.size());
        }
        long checksum = 0;
        int count = 0;
        try (DictionaryEntryIterator stream = dictionary.openEntryStream(config.batchSize)) {
            while (count < config.consumed() && stream.hasNext()) {
                checksum += checksum(stream.next());
                count++;
            }
            if (config.arm.equals("stream") && (count != config.consumed() || stream.hasNext())) {
                throw new IllegalStateException("stream cardinality differs from the generated corpus");
            }
        }
        return new Drain(checksum, count);
    }

    private static long checksum(DictionaryEntry entry) {
        if (entry.key().domain() != DictionaryUnitDomain.BYTE) {
            throw new IllegalStateException("benchmark expected byte-domain entries");
        }
        return entry.key().unitCount()
                ^ entry.value().map(io.vinarytree.interop.UnsignedLong::bits).orElse(0L);
    }

    private static void verify(Drain result, int count, long checksum, String phase) {
        if (result.count != count || result.checksum != checksum) {
            throw new IllegalStateException(phase + " checksum or cardinality mismatch");
        }
    }
}

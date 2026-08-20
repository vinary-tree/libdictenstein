package io.vinarytree.libdictenstein;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import java.util.OptionalLong;
import io.vinarytree.interop.DictionaryEntryIterator;
import io.vinarytree.interop.DictionaryKey;
import io.vinarytree.interop.DictionarySnapshot;
import io.vinarytree.interop.UnsignedLong;
import org.junit.jupiter.api.Test;

final class CollectionTraversalTest {
    @Test
    void byteSnapshotOwnsOrderedCollectionViews() {
        DictionarySnapshot snapshot;
        try (var dictionary = new DynamicDawg(UnitDomain.BYTE)) {
            dictionary.put(new byte[] {(byte) 0xff}, OptionalLong.of(-1L));
            dictionary.put(new byte[] {0}, OptionalLong.empty());
            dictionary.put(new byte[] {1}, OptionalLong.of(0));
            snapshot = dictionary.snapshot();
            dictionary.remove(new byte[] {0});
            dictionary.put(new byte[] {2}, OptionalLong.of(2));
        }
        assertEquals(3, snapshot.size());
        assertArrayEquals(new byte[] {0}, snapshot.orderedEntries().get(0).key().bytes());
        assertArrayEquals(new byte[] {1}, snapshot.orderedEntries().get(1).key().bytes());
        assertArrayEquals(new byte[] {(byte) 0xff}, snapshot.orderedEntries().get(2).key().bytes());
        assertTrue(snapshot.orderedEntries().get(0).value().isEmpty());
        assertEquals(new UnsignedLong(0), snapshot.orderedEntries().get(1).value().orElseThrow());
        assertEquals(new UnsignedLong(-1L), snapshot.orderedEntries().get(2).value().orElseThrow());
        assertEquals(snapshot.size(), snapshot.keys().size());
        assertEquals(snapshot.size(), snapshot.entries().values().size());
        assertEquals(snapshot.size(), snapshot.entries().size());
        assertThrows(UnsupportedOperationException.class, snapshot::clear);
        assertThrows(UnsupportedOperationException.class,
                () -> snapshot.entries().put(DictionaryKey.bytes(new byte[] {9}), java.util.Optional.empty()));
    }

    @Test
    void cursorRetainsRevisionAcrossMutationAndProducerClose() {
        var dictionary = new DynamicDawg(UnitDomain.UNICODE_SCALAR);
        dictionary.put("é", OptionalLong.of(7));
        dictionary.put("e", OptionalLong.empty());
        DictionaryEntryIterator stream = dictionary.openEntryStream(1);
        assertEquals(OptionalLong.of(2), stream.metadata().exactLength());
        assertTrue(stream.metadata().snapshotIdentity().isPresent());
        dictionary.put("z", OptionalLong.of(9));
        dictionary.close();
        try (stream) {
            List<String> keys = new java.util.ArrayList<>();
            stream.forEachRemaining(entry -> keys.add(entry.key().unicode()));
            assertEquals(List.of("e", "é"), keys);
            assertFalse(stream.hasNext());
        }
        stream.close();
    }

    @Test
    void u64KeysUseValueSemanticsAndJavaStreamsClose() {
        try (var dictionary = new DynamicDawg(UnitDomain.U64)) {
            dictionary.put(new long[] {0}, OptionalLong.empty());
            dictionary.put(new long[] {Long.MIN_VALUE}, OptionalLong.of(8));
            dictionary.put(new long[] {-1L}, OptionalLong.of(-1L));
            DictionarySnapshot snapshot = dictionary.snapshot();
            assertEquals(3, snapshot.entries().size());
            assertEquals(java.util.Optional.of(new UnsignedLong(8)),
                    snapshot.entries().get(DictionaryKey.u64(new long[] {Long.MIN_VALUE})));
            assertArrayEquals(new long[] {0}, snapshot.orderedEntries().get(0).key().u64());
            try (var entries = dictionary.streamEntries(1)) {
                assertEquals(1, entries.limit(1).count());
            }
        }
    }
}

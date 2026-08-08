package io.vinarytree.libdictenstein;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.vinarytree.liblevenshtein.Match;
import io.vinarytree.liblevenshtein.Transducer;
import io.vinarytree.liblevenshtein.Term;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.OptionalLong;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

final class BackendIntegrationTest {
    @Test
    void doubleArrayTrieComposesWithoutSerialization() {
        try (var dictionary = new DoubleArrayTrie(Map.of(
                "café", OptionalLong.of(7),
                "caff", OptionalLong.empty(),
                "tea", OptionalLong.of(9)));
             var transducer = new Transducer(dictionary);
             var cursor = transducer.query("cafe", 2)) {
            List<Match> matches = new ArrayList<>();
            cursor.forEachRemaining(matches::add);
            matches.sort((left, right) -> text(left).compareTo(text(right)));
            assertEquals(List.of("caff", "café"),
                    matches.stream().map(BackendIntegrationTest::text).toList());
            assertEquals(OptionalLong.empty(), matches.get(0).id());
            assertEquals(OptionalLong.of(7), matches.get(1).id());
        }
    }

    @Test
    void scdawgCursorRetainsOneRevision() {
        try (var dictionary = new Scdawg()) {
            assertEquals(3, dictionary.putAllStrings(Map.of(
                    "cat", OptionalLong.of(1),
                    "cot", OptionalLong.of(2),
                    "cut", OptionalLong.empty())));
            assertTrue(dictionary.containsSubstring("ot"));
            assertEquals(3, dictionary.frequency("t"));
            try (var transducer = new Transducer(dictionary);
                 var cursor = transducer.query("cat", 2)) {
                Match first = cursor.next();
                assertTrue(dictionary.put("cit", OptionalLong.of(4)));
                List<Match> frozen = new ArrayList<>();
                frozen.add(first);
                cursor.forEachRemaining(frozen::add);
                assertFalse(frozen.stream().anyMatch(match -> text(match).equals("cit")));
            }
        }
    }

    @Test
    void persistentCheckpointReopenAndCursorSnapshot(@TempDir Path directory) {
        Path path = directory.resolve("terms.part");
        var dictionary = PersistentARTrie.create(path);
        assertEquals(3, dictionary.putAllStrings(Map.of(
                "cat", OptionalLong.of(1),
                "cot", OptionalLong.of(2),
                "cut", OptionalLong.empty())));
        dictionary.checkpoint();
        try (var transducer = new Transducer(dictionary);
             var cursor = transducer.query("cat", 2)) {
            Match first = cursor.next();
            assertTrue(dictionary.remove("cot"));
            assertFalse(dictionary.put("cut", OptionalLong.of(30)));
            assertTrue(dictionary.put("cit", OptionalLong.of(4)));
            dictionary.checkpoint();
            List<Match> frozen = new ArrayList<>();
            frozen.add(first);
            cursor.forEachRemaining(frozen::add);
            frozen.sort((left, right) -> text(left).compareTo(text(right)));
            assertEquals(List.of("cat", "cot", "cut"),
                    frozen.stream().map(BackendIntegrationTest::text).toList());
        }
        dictionary.close();

        try (var reopened = PersistentARTrie.open(path)) {
            assertFalse(reopened.contains("cot"));
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(30)), reopened.get("cut"));
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(4)), reopened.get("cit"));
        }
    }

    @Test
    void persistentVocabularyRoundTrip(@TempDir Path directory) {
        Path path = directory.resolve("terms.vocab");
        try (var vocabulary = PersistentVocabulary.create(path)) {
            assertTrue(vocabulary.put("alpha", 41));
            assertTrue(vocabulary.put("beta", 42));
            assertEquals("alpha", vocabulary.term(41).orElseThrow());
            vocabulary.checkpoint();
        }
        try (var reopened = PersistentVocabulary.open(path)) {
            assertEquals(new Dictionary.Lookup(true, OptionalLong.of(42)), reopened.get("beta"));
            assertEquals("alpha", reopened.term(41).orElseThrow());
        }
    }

    private static String text(Match match) {
        return ((Term.Utf8) match.term()).value();
    }
}

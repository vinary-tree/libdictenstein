package io.vinarytree.libdictenstein;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.vinarytree.liblevenshtein.Match;
import io.vinarytree.liblevenshtein.Transducer;
import java.util.ArrayList;
import java.util.List;
import java.util.OptionalLong;
import org.junit.jupiter.api.Test;

final class CrossProjectSnapshotTest {
    @Test
    void longLivedCursorUsesOneRealLibdictensteinRevision() {
        DynamicDawg dictionary = new DynamicDawg();
        assertTrue(dictionary.put("cat", OptionalLong.of(1)));
        assertTrue(dictionary.put("cot", OptionalLong.of(2)));
        assertTrue(dictionary.put("cut", OptionalLong.of(3)));
        assertTrue(dictionary.put("scat", OptionalLong.empty()));

        Transducer transducer = new Transducer(dictionary);
        var cursor = transducer.query("cat", 2);
        Match first = cursor.next();

        assertTrue(dictionary.remove("cot"));
        assertFalse(dictionary.put("cut", OptionalLong.of(30)));
        assertTrue(dictionary.put("cit", OptionalLong.of(5)));
        dictionary.compact();
        dictionary.clear();
        assertTrue(dictionary.put("new", OptionalLong.of(99)));
        long freshCount = 0;
        try (var fresh = transducer.query("cat", 8)) {
            while (fresh.hasNext()) {
                fresh.next();
                freshCount++;
            }
        }
        assertEquals(1, freshCount);

        transducer.close();
        dictionary.close();
        List<Match> old = new ArrayList<>();
        old.add(first);
        cursor.forEachRemaining(old::add);
        assertEquals(4, old.size());
    }
}

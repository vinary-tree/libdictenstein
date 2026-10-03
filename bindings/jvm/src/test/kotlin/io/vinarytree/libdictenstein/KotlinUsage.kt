package io.vinarytree.libdictenstein

import io.vinarytree.interop.DictionaryEntry
import java.util.OptionalLong

/** Compile-checked Kotlin consumption of the shipped Java facade. */
object KotlinUsage {
    fun unionEntries(): List<DictionaryEntry> =
        DynamicDawg().use { left ->
            DynamicDawg().use { right ->
                left.put("cat", OptionalLong.of(7L))
                right.put("cot", OptionalLong.empty())
                left.union(right).use { combined ->
                    combined.entriesSnapshot().orderedEntries()
                }
            }
        }
}

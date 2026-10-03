# Use libdictenstein from Kotlin

The RC.6 Maven coordinate `io.vinarytree:libdictenstein:4.0.0-rc.6`
contains Java classes, not a separately published Kotlin wrapper. Kotlin
uses those classes directly, including `AutoCloseable` ownership and the
collection views supplied by `vinary-tree-interop`.

```kotlin
import io.vinarytree.interop.DictionaryEntry
import io.vinarytree.libdictenstein.DynamicDawg
import java.util.OptionalLong

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
```

`use` closes each native handle even on exceptions. `union` runs a native
ordered merge over two captured input revisions and returns an independent
mutable DynamicDAWG. The materialized `orderedEntries()` list owns its keys;
it remains valid after all three dictionaries close. The empty `OptionalLong`
means a present, valueless term—not an absent key. For a large dictionary,
prefer the bounded, closeable `entryStream()` over materializing all entries.

The [JVM guide](README.md) explains domains, persistence, capabilities, and
resource handoff. After RC.6 publication, the shared Java API reference will
be the artifact's
[Javadoc](https://javadoc.io/doc/io.vinarytree/libdictenstein/4.0.0-rc.6).
The [compile-checked source](src/test/kotlin/io/vinarytree/libdictenstein/KotlinUsage.kt)
is built by Gradle's `testClasses` task; the published JAR has no Kotlin
runtime dependency.

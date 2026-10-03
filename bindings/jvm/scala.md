# Use libdictenstein from Scala 3

The RC.6 Maven coordinate `io.vinarytree:libdictenstein:4.0.0-rc.6`
publishes one Java facade. Scala 3 consumes it directly; there is no
additional Scala wrapper or artifact.

```scala
import io.vinarytree.interop.DictionaryEntry
import io.vinarytree.libdictenstein.DynamicDawg
import java.util.OptionalLong
import scala.jdk.CollectionConverters.*
import scala.util.Using

def entries(): Vector[DictionaryEntry] =
  Using.resource(new DynamicDawg()): dictionary =>
    dictionary.put("cat", OptionalLong.of(7L))
    dictionary.put("cot", OptionalLong.empty())
    dictionary.entriesSnapshot().orderedEntries().asScala.toVector
```

`Using.resource` closes the native dictionary deterministically. The
materialized vector is independent of its captured dictionary revision and
survives closure. `OptionalLong.empty()` represents a present term without a
value; use the snapshot map's membership test to distinguish that state from
absence. For large datasets, use the closeable `entryStream()` and consume
bounded batches rather than materializing a complete snapshot.

The [JVM guide](README.md) covers persistent backends, algebraic operations,
domains, and capability checks. After RC.6 publication, the normative API
reference will be the artifact's
[Javadoc](https://javadoc.io/doc/io.vinarytree/libdictenstein/4.0.0-rc.6).
Gradle compiles the
[checked source](src/test/scala/io/vinarytree/libdictenstein/ScalaUsage.scala)
as part of `testClasses`; the Java artifact pulls in no Scala runtime.

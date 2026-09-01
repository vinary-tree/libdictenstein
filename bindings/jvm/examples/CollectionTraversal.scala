import io.vinarytree.libdictenstein.Dictionary
import io.vinarytree.interop.DictionaryEntry
import scala.jdk.CollectionConverters.*
import scala.util.Using

object CollectionTraversal:
  // The materialized snapshot adapts to a repeatable Scala collection.
  def entries(dictionary: Dictionary): Iterable[DictionaryEntry] =
    dictionary.snapshot().asScala

  // Using closes an early-terminated native iterator deterministically.
  def usingEntries[A](dictionary: Dictionary, batchSize: Int = 256)(
      consume: Iterator[DictionaryEntry] => A
  ): A =
    Using.resource(dictionary.openEntryStream(batchSize)) { cursor =>
      consume(cursor.asScala)
    }

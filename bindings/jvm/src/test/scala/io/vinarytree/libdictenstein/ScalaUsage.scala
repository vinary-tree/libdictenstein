package io.vinarytree.libdictenstein

import io.vinarytree.interop.DictionaryEntry
import java.util.OptionalLong
import scala.jdk.CollectionConverters.*
import scala.util.Using

/** Compile-checked Scala 3 consumption of the shipped Java facade. */
object ScalaUsage:
  def entries(): Vector[DictionaryEntry] =
    Using.resource(new DynamicDawg()): dictionary =>
      dictionary.put("cat", OptionalLong.of(7L))
      dictionary.put("cot", OptionalLong.empty())
      dictionary.entriesSnapshot().orderedEntries().asScala.toVector

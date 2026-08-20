import io.vinarytree.libdictenstein.Dictionary
import io.vinarytree.interop.DictionaryEntry

// Ordinary Kotlin collections/sequences are host-owned and repeatable.
fun Dictionary.entriesSequence(): Sequence<DictionaryEntry> = snapshot().asSequence()

// The native-backed alternative is deliberately lexical and single-pass.
inline fun <R> Dictionary.useEntrySequence(
    batchSize: Int = 256,
    block: (Sequence<DictionaryEntry>) -> R,
): R = openEntryStream(batchSize).use { cursor -> block(cursor.asSequence()) }

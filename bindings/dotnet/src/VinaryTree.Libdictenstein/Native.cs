using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
using VinaryTree.Interop;

namespace VinaryTree.Libdictenstein;

internal enum NativeStatus : uint
{
    Ok = 0, End = 1, InvalidArgument = 2, InvalidUtf8 = 3, NullPointer = 4,
    Panic = 5, Unsupported = 6, IoError = 7, Closed = 8,
    DomainMismatch = 9, LimitExceeded = 10,
}

[StructLayout(LayoutKind.Sequential)]
internal struct OptionalU64
{
    internal ulong Value;
    internal byte HasValue;
    private byte R1, R2, R3, R4, R5, R6, R7;
    internal static OptionalU64 From(ulong? value) => new() { Value = value.GetValueOrDefault(), HasValue = (byte)(value.HasValue ? 1 : 0) };
}

[StructLayout(LayoutKind.Sequential)]
internal unsafe struct TextEntry
{
    internal byte* Data;
    internal nuint Length;
    internal OptionalU64 Value;
}

internal sealed class DictionaryHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    internal DictionaryHandle(nint value) : base(true) => SetHandle(value);
    protected override bool ReleaseHandle() { Native.DictionaryFree(handle); return true; }
}

internal static unsafe partial class Native
{
    private const string Library = "libdictenstein";
    [LibraryImport(Library, EntryPoint = "ldict_abi_version")]
    internal static partial uint AbiVersion();
    [LibraryImport(Library, EntryPoint = "ldict_api_revision")]
    internal static partial uint ApiRevision();
    [LibraryImport(Library, EntryPoint = "ldict_last_error_message")]
    internal static partial nint LastError();
    [LibraryImport(Library, EntryPoint = "ldict_dynamic_dawg_new")]
    internal static partial NativeStatus DynamicNew(uint domain, out nint dictionary);
    [LibraryImport(Library, EntryPoint = "ldict_double_array_trie_new")]
    internal static partial NativeStatus DatNew(uint domain, TextEntry* entries, nuint count, out nint dictionary);
    [LibraryImport(Library, EntryPoint = "ldict_scdawg_new")]
    internal static partial NativeStatus ScdawgNew(uint domain, out nint dictionary);
    [LibraryImport(Library, EntryPoint = "ldict_persistent_artrie_create")]
    internal static partial NativeStatus PersistentCreate(uint domain, byte* path, nuint length, out nint dictionary);
    [LibraryImport(Library, EntryPoint = "ldict_persistent_artrie_open")]
    internal static partial NativeStatus PersistentOpen(uint domain, byte* path, nuint length, out nint dictionary);
    [LibraryImport(Library, EntryPoint = "ldict_persistent_vocab_create")]
    internal static partial NativeStatus VocabCreate(byte* path, nuint length, out nint dictionary);
    [LibraryImport(Library, EntryPoint = "ldict_persistent_vocab_open")]
    internal static partial NativeStatus VocabOpen(byte* path, nuint length, out nint dictionary);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_free")]
    internal static partial void DictionaryFree(nint dictionary);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_resource")]
    internal static partial NativeStatus Resource(DictionaryHandle dictionary, NativeResource* resource);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_kind")]
    internal static partial NativeStatus Kind(DictionaryHandle dictionary, out uint kind);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_capabilities")]
    internal static partial NativeStatus Capabilities(DictionaryHandle dictionary, out ulong capabilities);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_len")]
    internal static partial NativeStatus Length(DictionaryHandle dictionary, out nuint length);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_insert_text")]
    internal static partial NativeStatus InsertText(DictionaryHandle dictionary, byte* data, nuint length, OptionalU64 value, out byte inserted);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_remove_text")]
    internal static partial NativeStatus RemoveText(DictionaryHandle dictionary, byte* data, nuint length, out byte removed);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_contains_text")]
    internal static partial NativeStatus ContainsText(DictionaryHandle dictionary, byte* data, nuint length, out byte contains);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_get_text")]
    internal static partial NativeStatus GetText(DictionaryHandle dictionary, byte* data, nuint length, out byte found, out OptionalU64 value);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_insert_u64")]
    internal static partial NativeStatus InsertU64(DictionaryHandle dictionary, ulong* data, nuint length, OptionalU64 value, out byte inserted);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_remove_u64")]
    internal static partial NativeStatus RemoveU64(DictionaryHandle dictionary, ulong* data, nuint length, out byte removed);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_contains_u64")]
    internal static partial NativeStatus ContainsU64(DictionaryHandle dictionary, ulong* data, nuint length, out byte contains);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_get_u64")]
    internal static partial NativeStatus GetU64(DictionaryHandle dictionary, ulong* data, nuint length, out byte found, out OptionalU64 value);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_insert_text_batch")]
    internal static partial NativeStatus InsertTextBatch(DictionaryHandle dictionary, TextEntry* entries, nuint count, out nuint inserted);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_clear")]
    internal static partial NativeStatus Clear(DictionaryHandle dictionary);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_compact")]
    internal static partial NativeStatus Compact(DictionaryHandle dictionary, out nuint reclaimed);
    [LibraryImport(Library, EntryPoint = "ldict_dictionary_checkpoint")]
    internal static partial NativeStatus Checkpoint(DictionaryHandle dictionary);
    [LibraryImport(Library, EntryPoint = "ldict_scdawg_contains_substring")]
    internal static partial NativeStatus ContainsSubstring(DictionaryHandle dictionary, byte* data, nuint length, out byte contains);
    [LibraryImport(Library, EntryPoint = "ldict_scdawg_substring_frequency")]
    internal static partial NativeStatus SubstringFrequency(DictionaryHandle dictionary, byte* data, nuint length, out nuint frequency);
    [LibraryImport(Library, EntryPoint = "ldict_vocab_get_term")]
    internal static partial NativeStatus VocabTerm(DictionaryHandle dictionary, ulong index, byte* output, nuint capacity, out nuint length, out byte found);

    internal static void Check(NativeStatus status)
    {
        if (status == NativeStatus.Ok) return;
        string message = Marshal.PtrToStringUTF8(LastError()) ?? status.ToString();
        throw new LibdictensteinException((uint)status, message);
    }
}

/// <summary>A native dictionary binding error.</summary>
public sealed class LibdictensteinException : Exception
{
    internal LibdictensteinException(uint status, string message) : base(message) => StatusCode = status;
    /// <summary>The stable native status code.</summary>
    public uint StatusCode { get; }
}

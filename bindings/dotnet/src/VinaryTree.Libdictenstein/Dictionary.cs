using System.Text;
using VinaryTree.Interop;

namespace VinaryTree.Libdictenstein;

/// <summary>Transition unit domain.</summary>
public enum UnitDomain : uint
{
    /// <summary>Raw bytes.</summary>
    Byte = 1,
    /// <summary>Unicode scalar values.</summary>
    UnicodeScalar = 2,
    /// <summary>Unsigned 64-bit tokens.</summary>
    U64 = 3,
}

/// <summary>Concrete dictionary implementation.</summary>
public enum BackendKind : uint
{
    /// <summary>Mutable dynamic directed acyclic word graph.</summary>
    DynamicDawg = 1,
    /// <summary>Immutable double-array trie.</summary>
    DoubleArrayTrie = 2,
    /// <summary>Suffix-compressed directed acyclic word graph.</summary>
    Scdawg = 3,
    /// <summary>Filesystem-backed adaptive radix trie.</summary>
    PersistentArtrie = 4,
    /// <summary>Bidirectional persistent term/index vocabulary.</summary>
    PersistentVocabulary = 5,
}

/// <summary>Distinguishes absent terms, present terms without values, and mapped terms.</summary>
public readonly record struct Lookup(bool Found, ulong? Value);

/// <summary>Common exact-lookup and resource API for all backends.</summary>
public abstract class Dictionary : IDictionaryResource, IDisposable
{
    internal DictionaryHandle Handle { get; }
    internal Dictionary(nint handle) => Handle = handle != 0 ? new DictionaryHandle(handle) : throw new InvalidOperationException("native dictionary was not returned");

    /// <summary>Native ABI version (LDICT_ABI_VERSION); always 1 for this family.</summary>
    public static uint AbiVersion => Native.AbiVersion();
    /// <summary>Compatible-additions revision within the ABI version (LDICT_API_REVISION).</summary>
    public static uint ApiRevision => Native.ApiRevision();

    /// <summary>The concrete backend.</summary>
    public BackendKind Kind { get { Native.Check(Native.Kind(Handle, out uint result)); return (BackendKind)result; } }
    /// <summary>The stable capability bit set.</summary>
    public ulong Capabilities { get { Native.Check(Native.Capabilities(Handle, out ulong result)); return result; } }
    /// <summary>Number of complete terms.</summary>
    public nuint Count { get { Native.Check(Native.Length(Handle, out nuint result)); return result; } }

    /// <summary>Test a Unicode or raw-byte term.</summary>
    public unsafe bool Contains(string term)
    {
        byte[] text = Encoding.UTF8.GetBytes(term);
        fixed (byte* data = text) { Native.Check(Native.ContainsText(Handle, data, (nuint)text.Length, out byte result)); return result != 0; }
    }
    /// <summary>Look up a Unicode or raw-byte term.</summary>
    public unsafe Lookup Get(string term)
    {
        byte[] text = Encoding.UTF8.GetBytes(term);
        fixed (byte* data = text)
        {
            Native.Check(Native.GetText(Handle, data, (nuint)text.Length, out byte found, out OptionalU64 value));
            return new(found != 0, value.HasValue != 0 ? value.Value : null);
        }
    }
    /// <summary>Test a u64-token term.</summary>
    public unsafe bool Contains(ReadOnlySpan<ulong> term)
    {
        fixed (ulong* data = term) { Native.Check(Native.ContainsU64(Handle, data, (nuint)term.Length, out byte result)); return result != 0; }
    }
    /// <summary>Look up a u64-token term.</summary>
    public unsafe Lookup Get(ReadOnlySpan<ulong> term)
    {
        fixed (ulong* data = term)
        {
            Native.Check(Native.GetU64(Handle, data, (nuint)term.Length, out byte found, out OptionalU64 value));
            return new(found != 0, value.HasValue != 0 ? value.Value : null);
        }
    }

    /// <inheritdoc />
    public unsafe TResult WithResource<TResult>(ResourceCallback<TResult> callback)
    {
        ArgumentNullException.ThrowIfNull(callback);
        bool retained = false;
        try
        {
            Handle.DangerousAddRef(ref retained);
            NativeResource resource = default;
            Native.Check(Native.Resource(Handle, &resource));
            return callback(&resource);
        }
        finally { if (retained) Handle.DangerousRelease(); }
    }

    internal static unsafe bool InsertText(DictionaryHandle handle, string term, ulong? value)
    {
        byte[] text = Encoding.UTF8.GetBytes(term);
        fixed (byte* data = text) { Native.Check(Native.InsertText(handle, data, (nuint)text.Length, OptionalU64.From(value), out byte inserted)); return inserted != 0; }
    }
    internal static unsafe bool RemoveText(DictionaryHandle handle, string term)
    {
        byte[] text = Encoding.UTF8.GetBytes(term);
        fixed (byte* data = text) { Native.Check(Native.RemoveText(handle, data, (nuint)text.Length, out byte removed)); return removed != 0; }
    }
    internal static unsafe bool InsertTokens(DictionaryHandle handle, ReadOnlySpan<ulong> term, ulong? value)
    {
        fixed (ulong* data = term) { Native.Check(Native.InsertU64(handle, data, (nuint)term.Length, OptionalU64.From(value), out byte inserted)); return inserted != 0; }
    }
    internal static unsafe bool RemoveTokens(DictionaryHandle handle, ReadOnlySpan<ulong> term)
    {
        fixed (ulong* data = term) { Native.Check(Native.RemoveU64(handle, data, (nuint)term.Length, out byte removed)); return removed != 0; }
    }

    /// <inheritdoc />
    public void Dispose() => Handle.Dispose();
}

internal static class TextBatch
{
    internal unsafe delegate TResult DescriptorAction<TResult>(TextEntry* entries, nuint count);

    internal static unsafe TResult WithDescriptors<TResult>(IReadOnlyCollection<KeyValuePair<string, ulong?>> entries, DescriptorAction<TResult> action)
    {
        var encoded = new byte[entries.Count][];
        int total = 0;
        int encodedIndex = 0;
        foreach (KeyValuePair<string, ulong?> entry in entries)
        {
            encoded[encodedIndex] = Encoding.UTF8.GetBytes(entry.Key);
            total = checked(total + encoded[encodedIndex].Length);
            encodedIndex++;
        }
        byte[] arena = new byte[Math.Max(1, total)];
        var descriptors = new TextEntry[entries.Count];
        fixed (byte* basePointer = arena)
        fixed (TextEntry* descriptorPointer = descriptors)
        {
            int offset = 0;
            int index = 0;
            foreach (KeyValuePair<string, ulong?> entry in entries)
            {
                encoded[index].CopyTo(arena, offset);
                descriptors[index] = new TextEntry { Data = basePointer + offset, Length = (nuint)encoded[index].Length, Value = OptionalU64.From(entry.Value) };
                offset += encoded[index].Length;
                index++;
            }
            return action(descriptorPointer, (nuint)descriptors.Length);
        }
    }
}

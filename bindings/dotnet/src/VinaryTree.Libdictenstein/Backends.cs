using System.Text;

namespace VinaryTree.Libdictenstein;

/// <summary>Mutable DynamicDAWG with full CRUD.</summary>
public sealed class DynamicDawg : Dictionary
{
    /// <summary>Create an empty dictionary.</summary>
    public DynamicDawg(UnitDomain domain = UnitDomain.UnicodeScalar) : base(Create(domain)) { }
    private static nint Create(UnitDomain domain) { Native.Check(Native.DynamicNew((uint)domain, out nint result)); return result; }
    /// <summary>Insert or update a text term.</summary>
    public bool Put(string term, ulong? value = null) => InsertText(Handle, term, value);
    /// <summary>Insert or update a token term.</summary>
    public bool Put(ReadOnlySpan<ulong> term, ulong? value = null) => InsertTokens(Handle, term, value);
    /// <summary>Remove a text term.</summary>
    public bool Remove(string term) => RemoveText(Handle, term);
    /// <summary>Remove a token term.</summary>
    public bool Remove(ReadOnlySpan<ulong> term) => RemoveTokens(Handle, term);
    /// <summary>Insert a contiguous descriptor batch.</summary>
    public unsafe nuint PutAll(IReadOnlyCollection<KeyValuePair<string, ulong?>> entries) =>
        TextBatch.WithDescriptors(entries, (descriptors, count) => { Native.Check(Native.InsertTextBatch(Handle, descriptors, count, out nuint inserted)); return inserted; });
    /// <summary>Remove all terms.</summary>
    public void Clear() => Native.Check(Native.Clear(Handle));
    /// <summary>Compact unreachable storage.</summary>
    public nuint Compact() { Native.Check(Native.Compact(Handle, out nuint reclaimed)); return reclaimed; }
}

/// <summary>Immutable double-array trie built from one descriptor batch.</summary>
public sealed class DoubleArrayTrie : Dictionary
{
    /// <summary>Build an immutable trie.</summary>
    public unsafe DoubleArrayTrie(IReadOnlyCollection<KeyValuePair<string, ulong?>> entries, UnitDomain domain = UnitDomain.UnicodeScalar)
        : base(TextBatch.WithDescriptors(entries, (descriptors, count) => { Native.Check(Native.DatNew((uint)domain, descriptors, count, out nint result)); return result; })) { }
}

/// <summary>Incremental suffix dictionary supporting substring queries.</summary>
public sealed class Scdawg : Dictionary
{
    /// <summary>Create an empty suffix dictionary.</summary>
    public Scdawg(UnitDomain domain = UnitDomain.UnicodeScalar) : base(Create(domain)) { }
    private static nint Create(UnitDomain domain) { Native.Check(Native.ScdawgNew((uint)domain, out nint result)); return result; }
    /// <summary>Insert or update a complete term.</summary>
    public bool Put(string term, ulong? value = null) => InsertText(Handle, term, value);
    /// <summary>Test whether any indexed term contains the substring.</summary>
    public unsafe bool ContainsSubstring(string substring)
    {
        byte[] text = Encoding.UTF8.GetBytes(substring);
        fixed (byte* data = text) { Native.Check(Native.ContainsSubstring(Handle, data, (nuint)text.Length, out byte result)); return result != 0; }
    }
    /// <summary>Count substring occurrences.</summary>
    public unsafe nuint SubstringFrequency(string substring)
    {
        byte[] text = Encoding.UTF8.GetBytes(substring);
        fixed (byte* data = text) { Native.Check(Native.SubstringFrequency(Handle, data, (nuint)text.Length, out nuint result)); return result; }
    }
}

/// <summary>Filesystem-backed adaptive radix trie.</summary>
public sealed class PersistentArtrie : Dictionary
{
    private PersistentArtrie(nint handle) : base(handle) { }
    /// <summary>Create a new persistent trie.</summary>
    public static PersistentArtrie Create(string path, UnitDomain domain = UnitDomain.UnicodeScalar) => new(Open(path, domain, true));
    /// <summary>Open an existing persistent trie.</summary>
    public static PersistentArtrie Open(string path, UnitDomain domain = UnitDomain.UnicodeScalar) => new(Open(path, domain, false));
    private static unsafe nint Open(string path, UnitDomain domain, bool create)
    {
        byte[] encoded = Encoding.UTF8.GetBytes(Path.GetFullPath(path));
        fixed (byte* data = encoded)
        {
            NativeStatus status = create ? Native.PersistentCreate((uint)domain, data, (nuint)encoded.Length, out nint result)
                                         : Native.PersistentOpen((uint)domain, data, (nuint)encoded.Length, out result);
            Native.Check(status); return result;
        }
    }
    /// <summary>Insert or update a text term.</summary>
    public bool Put(string term, ulong? value = null) => InsertText(Handle, term, value);
    /// <summary>Insert or update a token term.</summary>
    public bool Put(ReadOnlySpan<ulong> term, ulong? value = null) => InsertTokens(Handle, term, value);
    /// <summary>Remove a text term.</summary>
    public bool Remove(string term) => RemoveText(Handle, term);
    /// <summary>Remove a token term.</summary>
    public bool Remove(ReadOnlySpan<ulong> term) => RemoveTokens(Handle, term);
    /// <summary>Durably publish the current revision.</summary>
    public void Checkpoint() => Native.Check(Native.Checkpoint(Handle));
}

/// <summary>Persistent bidirectional term/index vocabulary.</summary>
public sealed class PersistentVocabulary : Dictionary
{
    private PersistentVocabulary(nint handle) : base(handle) { }
    /// <summary>Create a new vocabulary.</summary>
    public static PersistentVocabulary Create(string path) => new(Open(path, true));
    /// <summary>Open an existing vocabulary.</summary>
    public static PersistentVocabulary Open(string path) => new(Open(path, false));
    private static unsafe nint Open(string path, bool create)
    {
        byte[] encoded = Encoding.UTF8.GetBytes(Path.GetFullPath(path));
        fixed (byte* data = encoded)
        {
            NativeStatus status = create ? Native.VocabCreate(data, (nuint)encoded.Length, out nint result)
                                         : Native.VocabOpen(data, (nuint)encoded.Length, out result);
            Native.Check(status); return result;
        }
    }
    /// <summary>Associate a term with an index.</summary>
    public bool Put(string term, ulong index) => InsertText(Handle, term, index);
    /// <summary>Resolve an index to a term.</summary>
    public unsafe string? GetTerm(ulong index)
    {
        Native.Check(Native.VocabTerm(Handle, index, null, 0, out nuint length, out byte found));
        if (found == 0) return null;
        byte[] output = new byte[checked((int)length)];
        fixed (byte* data = output) Native.Check(Native.VocabTerm(Handle, index, data, length, out _, out found));
        return Encoding.UTF8.GetString(output);
    }
    /// <summary>Durably publish the current revision.</summary>
    public void Checkpoint() => Native.Check(Native.Checkpoint(Handle));
}

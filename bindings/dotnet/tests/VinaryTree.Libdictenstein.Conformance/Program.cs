// Uniform facade conformance suite for the .NET binding.
//
// Instantiates the family C1-C10 contract for .NET against a live libdictenstein
// shared library. Unlike the cross-project Program in the sibling test project
// this suite needs only libdictenstein and the canonical fixture, never a
// liblevenshtein transducer, so it pins the *producer* ABI in isolation.
//
//   C1  identity + kind/capabilities per backend
//   C2  idempotent Dispose + free-order independence
//   C3  reachable status arms (DOMAIN_MISMATCH, IO_ERROR) + message
//       (INVALID_UTF8 unrepresentable via the string API;
//        NULL_POINTER/UNSUPPORTED/LIMIT_EXCEEDED marked N/A with a reason)
//   C4  canonical fixture replay (all four backends)
//   C5  CRUD + value + batch + substring; capability-derived rejects
//   C6  precomposed/combining/multibyte, byte-domain NUL, u64 0/MAX
//   C7  batch sizes 0/1/255/256/257/1000
//   C8  CRUD op-script vs a Dictionary oracle; substring vs a naive oracle
//   C9  12k create/use/free cycles with /proc VmRSS bounded
//   C10 independent per-task dictionaries + concurrent readers during a writer

using System.Collections.Concurrent;
using System.Text.Json;
using VinaryTree.Interop;
using VinaryTree.Libdictenstein;

int failures = 0;
void Check(bool ok, string what)
{
    if (!ok)
    {
        Console.Error.WriteLine($"FAIL: {what}");
        failures++;
    }
}

// -- fixture (C4) ----------------------------------------------------------

string fixturePath = args.Length > 0 ? args[0] : "bindings/canonical_fixture.json";
if (!File.Exists(fixturePath))
{
    foreach (string candidate in new[] { "bindings/canonical_fixture.json", "../canonical_fixture.json", "../../canonical_fixture.json" })
    {
        if (File.Exists(candidate)) { fixturePath = candidate; break; }
    }
}

static ulong? OptionalValue(JsonElement element) =>
    element.ValueKind == JsonValueKind.Null ? null : element.GetUInt64();

using JsonDocument document = JsonDocument.Parse(File.ReadAllText(fixturePath));
JsonElement root = document.RootElement;

var entries = new List<KeyValuePair<string, ulong?>>();
foreach (JsonElement e in root.GetProperty("entries").EnumerateArray())
    entries.Add(new(e.GetProperty("term").GetString()!, OptionalValue(e.GetProperty("value"))));
int size = root.GetProperty("size").GetInt32();
var containsCases = new List<(string Term, bool Expected)>();
foreach (JsonElement e in root.GetProperty("contains").EnumerateArray())
    containsCases.Add((e.GetProperty("term").GetString()!, e.GetProperty("expected").GetBoolean()));
var getCases = new List<(string Term, bool Found, ulong? Value)>();
foreach (JsonElement e in root.GetProperty("get").EnumerateArray())
    getCases.Add((e.GetProperty("term").GetString()!, e.GetProperty("found").GetBoolean(), OptionalValue(e.GetProperty("value"))));
var freqCases = new List<(string Pattern, ulong Expected)>();
foreach (JsonElement e in root.GetProperty("substring_frequency").EnumerateArray())
    freqCases.Add((e.GetProperty("pattern").GetString()!, e.GetProperty("expected").GetUInt64()));
var substrCases = new List<(string Pattern, bool Expected)>();
foreach (JsonElement e in root.GetProperty("substring_contains").EnumerateArray())
    substrCases.Add((e.GetProperty("pattern").GetString()!, e.GetProperty("expected").GetBoolean()));

void AssertFixtureReads(Dictionary dictionary)
{
    Check((int)dictionary.Count == size, "fixture size");
    foreach ((string term, bool expected) in containsCases)
        Check(dictionary.Contains(term) == expected, $"contains {term}");
    foreach ((string term, bool found, ulong? value) in getCases)
    {
        Lookup lookup = dictionary.Get(term);
        Check(lookup.Found == found, $"get.found {term}");
        Check(lookup.Value == value, $"get.value {term}");
    }
}

// -- C1 identity/version ---------------------------------------------------

Check(Dictionary.AbiVersion == 1, "abi version == 1");
Check(Dictionary.ApiRevision == 5, "api revision == 5");

const ulong Read = 1UL << 0, Insert = 1UL << 1, Remove = 1UL << 2,
    Clear = 1UL << 3, Compact = 1UL << 4, Substring = 1UL << 5, Checkpoint = 1UL << 6;
using (var dawg = new DynamicDawg())
{
    Check(dawg.Kind == BackendKind.DynamicDawg, "dawg kind");
    ulong caps = dawg.Capabilities;
    Check((caps & Insert) != 0 && (caps & Remove) != 0 && (caps & Clear) != 0 && (caps & Compact) != 0, "dawg caps");
    Check((caps & Substring) == 0 && (caps & Checkpoint) == 0, "dawg lacks substring/checkpoint");
}
using (var dat0 = new DoubleArrayTrie(new Dictionary<string, ulong?> { ["x"] = null }))
{
    Check(dat0.Kind == BackendKind.DoubleArrayTrie, "dat kind");
    Check((dat0.Capabilities & Read) != 0, "dat read");
    Check((dat0.Capabilities & Insert) == 0, "dat lacks insert");
}
using (var scdawg0 = new Scdawg())
{
    Check(scdawg0.Kind == BackendKind.Scdawg, "scdawg kind");
    Check((scdawg0.Capabilities & Substring) != 0, "scdawg substring");
}

// -- C2 lifecycle/ownership ------------------------------------------------

{
    var dawg = new DynamicDawg();
    dawg.Put("a");
    dawg.Dispose();
    dawg.Dispose(); // idempotent
}
{
    var dicts = new List<DynamicDawg>();
    for (int i = 0; i < 4; i++) { var d = new DynamicDawg(); d.Put($"term{i}", (ulong)i); dicts.Add(d); }
    foreach (int index in new[] { 2, 0, 3, 1 }) dicts[index].Dispose(); // scrambled free order
}

// -- C3 error-mapping matrix + thread-local message ------------------------
// Reachable via the idiomatic API: DOMAIN_MISMATCH (9), IO_ERROR (7).
// N/A: INVALID_UTF8 (3) is unrepresentable through the string term API;
// NULL_POINTER (4) cannot be produced (SafeHandle guards); UNSUPPORTED (6) is
// capability-derived (C5); LIMIT_EXCEEDED (10) is auto-sized away by GetTerm.

{
    using var dawg = new DynamicDawg(UnitDomain.UnicodeScalar);
    try { dawg.Put(new ulong[] { 1, 2 }); Check(false, "domain mismatch throws"); }
    catch (LibdictensteinException error) { Check(error.StatusCode == 9, "domain mismatch status 9"); }
}
{
    string missing = Path.Combine(Path.GetTempPath(), $"ldict-dotnet-missing-{Guid.NewGuid():N}.part");
    try { PersistentArtrie.Open(missing); Check(false, "io error throws"); }
    catch (LibdictensteinException error)
    {
        Check(error.StatusCode == 7, "io error status 7");
        Check(error.Message.Length > 0, "io error message");
    }
}

// -- C4 canonical fixture replay -------------------------------------------

using (var dawg = new DynamicDawg())
{
    Check((int)dawg.PutAll(entries) == size, "dawg batch count");
    AssertFixtureReads(dawg);
}
using (var dat = new DoubleArrayTrie(entries))
{
    AssertFixtureReads(dat);
}
{
    string path = Path.Combine(Path.GetTempPath(), $"ldict-dotnet-c4-{Guid.NewGuid():N}.part");
    using (var art = PersistentArtrie.Create(path))
    {
        foreach (KeyValuePair<string, ulong?> entry in entries) art.Put(entry.Key, entry.Value);
        AssertFixtureReads(art);
    }
    foreach (string artifact in new[] { path, path + ".wal", path + ".wlock" })
        if (File.Exists(artifact)) File.Delete(artifact);
}
using (var scdawg = new Scdawg())
{
    foreach (KeyValuePair<string, ulong?> entry in entries) scdawg.Put(entry.Key, entry.Value);
    foreach ((string pattern, ulong expected) in freqCases)
        Check((ulong)scdawg.SubstringFrequency(pattern) == expected, $"frequency {pattern}");
    foreach ((string pattern, bool expected) in substrCases)
        Check(scdawg.ContainsSubstring(pattern) == expected, $"contains_substring {pattern}");
}

// -- C5 CRUD + value + batch + substring; capability-derived rejects -------

using (var dawg = new DynamicDawg())
{
    Check(dawg.Put("cat", 1), "insert cat");
    Check(!dawg.Put("cat", 1), "idempotent insert");
    Check(dawg.Get("cat").Value == 1, "get cat");
    Check(dawg.Remove("cat"), "remove cat");
    Check(!dawg.Remove("cat"), "second remove");
    Check(!dawg.Contains("cat"), "cat gone");
}
using (var dawg = new DynamicDawg())
{
    var batch = new List<KeyValuePair<string, ulong?>>();
    for (int i = 0; i < 50; i++) batch.Add(new($"t{i}", (ulong)i));
    dawg.PutAll(batch);
    for (int i = 0; i < 50; i += 2) Check(dawg.Remove($"t{i}"), $"remove t{i}");
    dawg.Compact();
    Check((int)dawg.Count == 25, "compact size");
    Check(dawg.Get("t1").Value == 1, "t1 survives");
    Check(!dawg.Contains("t0"), "t0 gone");
}
using (var scdawg = new Scdawg())
{
    scdawg.Put("cat", 1);
    scdawg.Put("cot", 2);
    Check((ulong)scdawg.SubstringFrequency("t") == 2, "freq t == 2");
    Check(scdawg.Put("cut"), "insert cut");
    Check((ulong)scdawg.SubstringFrequency("t") == 3, "freq t == 3");
}
using (var dat = new DoubleArrayTrie(new Dictionary<string, ulong?> { ["x"] = null }))
{
    Check((dat.Capabilities & (Insert | Remove | Clear | Compact)) == 0, "dat capability-derived reject");
}

// -- C6 text domains and values --------------------------------------------

using (var dawg = new DynamicDawg())
{
    Check(dawg.Put("café", 7), "precomposed insert");   // precomposed U+00E9
    Check(dawg.Put("🦀", 255), "emoji insert");     // 4-byte scalar
    Check(dawg.Contains("café"), "precomposed contains");
    Check(dawg.Get("🦀").Value == 255, "emoji value");
}
using (var dawg = new DynamicDawg())
{
    const string precomposed = "café";  // café with a precomposed U+00E9
    const string combining = "café";   // cafe + U+0301 combining acute
    Check(dawg.Put(precomposed, 1), "precomposed distinct insert");
    Check(dawg.Put(combining, 2), "combining distinct insert");
    Check((int)dawg.Count == 2, "distinct scalar sequences");
    Check(dawg.Get(precomposed).Value == 1, "precomposed value");
    Check(dawg.Get(combining).Value == 2, "combining value");
}
using (var dawg = new DynamicDawg(UnitDomain.Byte))
{
    const string embeddedNul = "a\0b";
    Check(dawg.Put(embeddedNul, 1), "embedded NUL insert");
    Check(dawg.Contains(embeddedNul), "embedded NUL contains");
    Check(dawg.Get(embeddedNul).Value == 1, "embedded NUL value");
}
using (var dawg = new DynamicDawg(UnitDomain.U64))
{
    Check(dawg.Put(new ulong[] { 1, 2, 3 }, 0), "u64 value 0 insert");
    Check(dawg.Put(new ulong[] { 9 }, ulong.MaxValue), "u64 value MAX insert");
    Check(dawg.Get(new ulong[] { 1, 2, 3 }).Value == 0, "u64 value 0");
    Check(dawg.Get(new ulong[] { 9 }).Value == ulong.MaxValue, "u64 value MAX");
}

// -- C7 batch / paging edges ----------------------------------------------

foreach (int batchSize in new[] { 0, 1, 255, 256, 257, 1000 })
{
    using var dawg = new DynamicDawg();
    var batch = new List<KeyValuePair<string, ulong?>>(batchSize);
    for (int i = 0; i < batchSize; i++) batch.Add(new($"t{i}", (ulong)i));
    Check((int)dawg.PutAll(batch) == batchSize, $"batch {batchSize} count");
    Check((int)dawg.Count == batchSize, $"batch {batchSize} size");
    if (batchSize > 0)
    {
        Check(dawg.Get("t0").Value == 0, $"batch {batchSize} first");
        Check(dawg.Get($"t{batchSize - 1}").Value == (ulong)(batchSize - 1), $"batch {batchSize} last");
    }
}

// -- C8 property-based testing vs an in-language oracle --------------------

{
    var rng = new Random(0x0C0FFEE);
    string[] keys = new string[40];
    for (int i = 0; i < keys.Length; i++) keys[i] = $"k{i}";
    var oracle = new Dictionary<string, ulong?>();
    using var dawg = new DynamicDawg();
    for (int step = 0; step < 3000; step++)
    {
        string key = keys[rng.Next(keys.Length)];
        bool present = oracle.ContainsKey(key);
        double op = rng.NextDouble();
        if (op < 0.5)
        {
            ulong? value = rng.Next(2) == 0 ? null : (ulong)rng.Next(0, int.MaxValue);
            Check(dawg.Put(key, value) == !present, "crud insert changed");
            oracle[key] = value;
        }
        else if (op < 0.75)
        {
            Check(dawg.Remove(key) == present, "crud remove changed");
            oracle.Remove(key);
        }
        else if (op < 0.95)
        {
            Check(dawg.Contains(key) == present, "crud contains");
            if (present) Check(dawg.Get(key).Value == oracle[key], "crud get value");
        }
        else
        {
            dawg.Compact();
        }
        Check((int)dawg.Count == oracle.Count, "crud size matches oracle");
    }
}
{
    var rng = new Random(0x5CDA);
    char[] alphabet = { 'a', 'b', 'c', 'x' };
    string Generate(int maxLen)
    {
        int n = 1 + rng.Next(maxLen);
        char[] buffer = new char[n];
        for (int i = 0; i < n; i++) buffer[i] = alphabet[rng.Next(alphabet.Length)];
        return new string(buffer);
    }
    var terms = new HashSet<string>();
    while (terms.Count < 60) terms.Add(Generate(6));
    using var scdawg = new Scdawg();
    foreach (string term in terms) scdawg.Put(term);
    for (int i = 0; i < 200; i++)
    {
        string pattern = Generate(3);
        ulong expected = 0;
        foreach (string term in terms)
            for (int start = 0; start + pattern.Length <= term.Length; start++)
                if (term.AsSpan(start, pattern.Length).SequenceEqual(pattern)) expected++;
        Check((ulong)scdawg.SubstringFrequency(pattern) == expected, $"pbt frequency {pattern}");
        Check(scdawg.ContainsSubstring(pattern) == (expected > 0), $"pbt contains {pattern}");
    }
}

// -- C9 leak discipline ----------------------------------------------------

static long RssKib()
{
    try
    {
        foreach (string line in File.ReadLines("/proc/self/status"))
            if (line.StartsWith("VmRSS:", StringComparison.Ordinal))
                return long.Parse(new string(line.Where(char.IsDigit).ToArray()));
    }
    catch (IOException) { /* not available */ }
    return 0;
}
{
    const int cycles = 12000;
    var batch = new List<KeyValuePair<string, ulong?>> { new("cat", 1), new("cot", 2), new("cut", null) };
    for (int warmup = 0; warmup < 2000; warmup++)
    {
        using var dawg = new DynamicDawg();
        dawg.Put("cat", 1);
    }
    GC.Collect();
    GC.WaitForPendingFinalizers();
    long before = RssKib();
    for (int i = 0; i < cycles; i++)
    {
        using var dawg = new DynamicDawg();
        dawg.PutAll(batch);
        Check(dawg.Contains("cot"), "leak cycle contains");
    }
    GC.Collect();
    GC.WaitForPendingFinalizers();
    long after = RssKib();
    if (before != 0 && after > before)
        Check(after - before < 96L * 1024L, $"RSS grew {after - before} KiB over {cycles} cycles");
}

// -- C10 concurrency -------------------------------------------------------

{
    var errors = new ConcurrentBag<Exception>();
    var workers = new List<Task>();
    for (int seed = 0; seed < 8; seed++)
    {
        int s = seed;
        workers.Add(Task.Run(() =>
        {
            try
            {
                using var dawg = new DynamicDawg();
                for (int i = 0; i < 2000; i++) dawg.Put($"t{s}_{i}", (ulong)i);
                if ((int)dawg.Count != 2000) throw new Exception("len");
                if (dawg.Get($"t{s}_1500").Value != 1500) throw new Exception("get");
            }
            catch (Exception failure) { errors.Add(failure); }
        }));
    }
    Task.WaitAll(workers.ToArray());
    Check(errors.IsEmpty, "independent per-task dictionaries");
}
{
    var errors = new ConcurrentBag<Exception>();
    using var dawg = new DynamicDawg();
    var seed = new List<KeyValuePair<string, ulong?>>();
    for (int i = 0; i < 500; i++) seed.Add(new($"seed{i}", (ulong)i));
    dawg.PutAll(seed);
    using var stop = new CancellationTokenSource();
    var readers = new List<Task>();
    for (int r = 0; r < 4; r++)
    {
        readers.Add(Task.Run(() =>
        {
            try
            {
                while (!stop.IsCancellationRequested)
                {
                    if (!dawg.Contains("seed0")) throw new Exception("lost seed0");
                    dawg.Get("seed250");
                }
            }
            catch (Exception failure) { errors.Add(failure); }
        }));
    }
    for (int i = 500; i < 3000; i++) dawg.Put($"w{i}", (ulong)i);
    stop.Cancel();
    Task.WaitAll(readers.ToArray());
    Check(errors.IsEmpty, "concurrent readers during writer");
    Check(dawg.Get("w2999").Value == 2999, "final write present");
}

// -- native collection protocols ------------------------------------------

{
    DictionarySnapshot snapshot;
    using (var dictionary = new DynamicDawg(UnitDomain.Byte))
    {
        dictionary.Put("z", ulong.MaxValue);
        dictionary.Put("a", null);
        dictionary.Put("b", 0);
        snapshot = dictionary.Snapshot();
        dictionary.Remove("a");
        dictionary.Put("c", 3);
    }
    Check(snapshot.Count == 3, "snapshot count");
    DictionaryEntry[] snapshotEntries = snapshot.ToArray();
    Check(snapshotEntries.Select(entry => entry.Key.ToByteArray()[0]).SequenceEqual(new byte[] { (byte)'a', (byte)'b', (byte)'z' }), "snapshot byte order");
    Check(snapshotEntries[0].Value is null, "snapshot valueless member");
    Check(snapshotEntries[1].Value == 0, "snapshot mapped zero");
    Check(snapshotEntries[2].Value == ulong.MaxValue, "snapshot mapped max");
    Check(snapshot.Keys.Count == 3 && snapshot.Entries.Values.Count() == 3, "snapshot projected views");
    Check(snapshot.Entries.ContainsKey(snapshotEntries[1].Key), "snapshot readonly dictionary lookup");
}
{
    var dictionary = new DynamicDawg();
    dictionary.Put("é", 7);
    dictionary.Put("e", null);
    using DictionaryEntryEnumerator stream = dictionary.OpenEntryStream(1);
    Check(stream.Metadata.ExactLength == 2 && stream.Metadata.SnapshotIdentity is not null, "entry metadata");
    dictionary.Put("z", 9);
    dictionary.Dispose();
    var keys = new List<string>();
    while (stream.MoveNext()) keys.Add(stream.Current.Key.ToUnicodeString());
    Check(keys.SequenceEqual(new[] { "e", "é" }), "cursor retains producer revision");
    Check(!stream.MoveNext(), "entry stream fused");
}
{
    using var dictionary = new DynamicDawg(UnitDomain.U64);
    dictionary.Put(new ulong[] { 0 }, null);
    dictionary.Put(new ulong[] { 1 }, 0);
    dictionary.Put(new ulong[] { ulong.MaxValue }, ulong.MaxValue);
    DictionarySnapshot snapshot = dictionary.Snapshot();
    DictionaryEntry[] snapshotEntries = snapshot.ToArray();
    Check(snapshotEntries.Select(entry => entry.Key.ToU64Array()[0]).SequenceEqual(new ulong[] { 0, 1, ulong.MaxValue }), "u64 snapshot order");
    Check(snapshot.Entries.TryGetValue(snapshotEntries[2].Key, out ulong? maximum) && maximum == ulong.MaxValue, "u64 value-semantic map lookup");
}

// -- summary ---------------------------------------------------------------

if (failures == 0)
{
    Console.WriteLine("dotnet conformance: all checks passed");
    return 0;
}
Console.Error.WriteLine($"dotnet conformance: {failures} check(s) failed");
return 1;

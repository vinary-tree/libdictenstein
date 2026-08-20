using System.Diagnostics;
using System.Text.Json;
using VinaryTree.Interop;
using VinaryTree.Libdictenstein;

const string Schema = "libdictenstein.host-collection-traversal.v1";

var config = Config.Parse(args);
var corpus = Enumerable.Range(0, config.Entries)
    .Select(index => new KeyValuePair<string, ulong?>(
        $"collection/{index & 0x0fff:x4}/{index:x8}/shared-suffix", (ulong)index))
    .ToList();
ulong expected = Checksum(corpus.OrderBy(pair => pair.Key, StringComparer.Ordinal)
    .Take(config.Consumed).Select(pair => ((ulong)pair.Key.Length, pair.Value)));

using var dictionary = new DynamicDawg(UnitDomain.Byte);
nuint inserted = dictionary.PutAll(corpus);
if (inserted != (nuint)corpus.Count)
    throw new InvalidOperationException($"inserted {inserted} of {corpus.Count} generated entries");

for (int pass = 0; pass < config.WarmupPasses; pass++) Verify(Drain(dictionary, config), "warmup");
long started = Stopwatch.GetTimestamp();
ulong checksum = 0;
for (int pass = 0; pass < config.Passes; pass++)
{
    (ulong passChecksum, int count) = Drain(dictionary, config);
    Verify((passChecksum, count), "timed drain");
    checksum = unchecked(checksum + passChecksum);
}
long ticks = Stopwatch.GetTimestamp() - started;
long elapsedNs = Math.Max(1, checked((long)(ticks * (1_000_000_000.0 / Stopwatch.Frequency))));
if (checksum != unchecked(expected * (ulong)config.Passes))
    throw new InvalidOperationException("aggregate checksum mismatch");

Console.WriteLine(JsonSerializer.Serialize(new
{
    schema = Schema,
    runtime = "dotnet",
    arm = config.Arm,
    dictionary_entries = config.Entries,
    consumed_entries_per_pass = config.Consumed,
    passes = config.Passes,
    warmup_passes = config.WarmupPasses,
    batch_size = config.Arm == "materialized" ? (int?)null : config.BatchSize,
    early_cancel = config.Arm == "stream-cancel" ? config.EarlyCancel : (int?)null,
    elapsed_ns = elapsedNs,
    checksum,
}));

void Verify((ulong Checksum, int Count) result, string phase)
{
    if (result != (expected, config.Consumed))
        throw new InvalidOperationException($"{phase} checksum or cardinality mismatch");
}

static ulong Checksum(IEnumerable<(ulong KeyLength, ulong? Value)> entries)
{
    ulong result = 0;
    foreach ((ulong keyLength, ulong? value) in entries)
        result = unchecked(result + (keyLength ^ value.GetValueOrDefault()));
    return result;
}

static (ulong Checksum, int Count) Drain(DynamicDawg dictionary, Config config)
{
    if (config.Arm == "materialized")
    {
        DictionarySnapshot snapshot = dictionary.Snapshot();
        return (Checksum(snapshot.Select(entry => ((ulong)entry.Key.UnitCount, entry.Value))), snapshot.Count);
    }
    ulong checksum = 0;
    int count = 0;
    using DictionaryEntryEnumerator stream = dictionary.OpenEntryStream(config.BatchSize);
    while (count < config.Consumed && stream.MoveNext())
    {
        DictionaryEntry entry = stream.Current;
        checksum = unchecked(checksum + ((ulong)entry.Key.UnitCount ^ entry.Value.GetValueOrDefault()));
        count++;
    }
    if (config.Arm == "stream" && (count != config.Consumed || stream.MoveNext()))
        throw new InvalidOperationException("stream cardinality differs from the generated corpus");
    return (checksum, count);
}

internal sealed record Config(string Arm, int Entries, int Passes, int WarmupPasses, int BatchSize, int EarlyCancel)
{
    private const int KeyUnits = 38;
    internal int Consumed => Arm == "stream-cancel" ? Math.Min(Entries, EarlyCancel) : Entries;

    internal static Config Parse(string[] arguments)
    {
        string? arm = null;
        int entries = 65_536, passes = 1, warmup = 1, batch = 256, early = 64;
        if (arguments.Length % 2 != 0) throw new ArgumentException("every option requires a value");
        for (int index = 0; index < arguments.Length; index += 2)
        {
            string option = arguments[index], value = arguments[index + 1];
            switch (option)
            {
                case "--arm": arm = value; break;
                case "--entries": entries = ParseInt(value, option); break;
                case "--passes": passes = ParseInt(value, option); break;
                case "--warmup-passes": warmup = ParseInt(value, option, true); break;
                case "--batch-size": batch = ParseInt(value, option); break;
                case "--early-cancel": early = ParseInt(value, option); break;
                default: throw new ArgumentException($"unknown argument: {option}");
            }
        }
        if (arm is not ("materialized" or "stream" or "stream-cancel"))
            throw new ArgumentException("--arm must be materialized, stream, or stream-cancel");
        _ = checked(batch * KeyUnits);
        return new(arm, entries, passes, warmup, batch, early);
    }

    private static int ParseInt(string value, string option, bool allowZero = false)
    {
        if (!int.TryParse(value, out int parsed) || (allowZero ? parsed < 0 : parsed <= 0))
            throw new ArgumentException($"{option} must be {(allowZero ? "nonnegative" : "positive")}");
        return parsed;
    }
}

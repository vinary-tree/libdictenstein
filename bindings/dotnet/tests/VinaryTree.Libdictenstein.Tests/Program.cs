using VinaryTree.Libdictenstein;
using VinaryTree.Liblevenshtein;

using var dictionary = new DynamicDawg();
dictionary.PutAll(new Dictionary<string, ulong?> { ["cat"] = 1, ["cot"] = 2, ["cut"] = 3, ["scat"] = null });
using var transducer = new Transducer(dictionary);
using Query query = transducer.Query("cat", 2);
using IEnumerator<Match> iterator = query.GetEnumerator();
if (!iterator.MoveNext()) throw new Exception("cursor unexpectedly empty");
var old = new HashSet<string> { (string)iterator.Current.Term };
dictionary.Remove("cot");
dictionary.Put("cut", 30);
dictionary.Put("cit", 5);
dictionary.Compact();
while (iterator.MoveNext()) old.Add((string)iterator.Current.Term);
if (!old.SetEquals(["cat", "cot", "cut", "scat"])) throw new Exception("query-start snapshot changed");

for (int seed = 0; seed < 64; seed++)
{
    using var subject = new DynamicDawg();
    for (int i = 0; i < 32; i++) subject.Put($"t{i:D2}", (ulong)i);
    using var machine = new Transducer(subject);
    using Query stream = machine.Query("t00", 3);
    using IEnumerator<Match> cursor = stream.GetEnumerator();
    var expected = new HashSet<string>();
    while (cursor.MoveNext()) expected.Add((string)cursor.Current.Term);
    using Query stream2 = machine.Query("t00", 3);
    using IEnumerator<Match> cursor2 = stream2.GetEnumerator();
    cursor2.MoveNext();
    subject.Remove($"t{seed % 32:D2}");
    subject.Put($"x{seed:D2}", (ulong)seed);
    var actual = new HashSet<string> { (string)cursor2.Current.Term };
    while (cursor2.MoveNext()) actual.Add((string)cursor2.Current.Term);
    if (!actual.SetEquals(expected)) throw new Exception($"property snapshot failed at seed {seed}");
}

using var dat = new DoubleArrayTrie(new Dictionary<string, ulong?> { ["alpha"] = 7, ["beta"] = null });
if (dat.Get("alpha").Value != 7) throw new Exception("DAT lookup failed");
using var suffix = new Scdawg();
suffix.Put("banana", 1);
if (!suffix.ContainsSubstring("ana") || suffix.SubstringFrequency("ana") != 2) throw new Exception("SCDAWG failed");

string persistentPath = Path.Combine(Path.GetTempPath(), $"vinary-tree-dotnet-{Guid.NewGuid():N}");
using (var persistent = PersistentArtrie.Create(persistentPath)) { persistent.Put("durable", 9); persistent.Checkpoint(); }
using (var reopened = PersistentArtrie.Open(persistentPath)) if (reopened.Get("durable").Value != 9) throw new Exception("persistent reopen failed");
foreach (string artifact in new[] { persistentPath, persistentPath + ".wal", persistentPath + ".wlock" })
    File.Delete(artifact);
Console.WriteLine(".NET cross-project snapshot conformance passed");

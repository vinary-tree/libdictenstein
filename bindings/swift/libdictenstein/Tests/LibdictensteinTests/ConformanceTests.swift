// Uniform facade conformance suite for the Swift binding.
//
// Instantiates the family C1-C10 contract for Swift against a live
// libdictenstein shared library. It needs only libdictenstein and the canonical
// fixture, never a liblevenshtein transducer, so it pins the *producer* ABI in
// isolation.
//
//   C1  identity + kind/capabilities per backend
//   C2  idempotent close + free-order independence
//   C3  error raised (+ non-empty description) for DOMAIN_MISMATCH / IO_ERROR
//       (INVALID_UTF8 is unrepresentable via the String term API; the error
//        carries only a message, so status-code granularity is N/A)
//   C4  canonical fixture replay (all four backends)
//   C5  CRUD + value + substring; capability-derived assertions
//   C6  precomposed/combining/multibyte, byte-domain NUL, u64 0/MAX
//   C7  batch sizes 0/1/255/256/257/1000 (DoubleArrayTrie construction)
//   C8  CRUD op-script vs a Dictionary oracle; substring vs a naive oracle
//   C9  leak discipline (>=10k cycles, RSS bounded)
//   C10 independent per-task dictionaries + concurrent readers during a writer
//
// Run (with the cdylib on the linker/loader path):
//   LIBRARY_PATH=target/release LD_LIBRARY_PATH=target/release \
//     LDICT_FIXTURE=$PWD/bindings/canonical_fixture.json \
//     swift test --package-path bindings/swift/libdictenstein

import Dispatch
import Foundation
import VinaryTreeInterop
import XCTest

@testable import Libdictenstein

// A seedable PRNG so the property tests are deterministic.
private struct LCG: RandomNumberGenerator {
    var state: UInt64
    init(seed: UInt64) { state = seed }
    mutating func next() -> UInt64 {
        state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
        return state
    }
}

private final class AtomicInt: @unchecked Sendable {
    private var value = 0
    private let lock = NSLock()
    func increment() { lock.lock(); value += 1; lock.unlock() }
    var current: Int { lock.lock(); defer { lock.unlock() }; return value }
}

private final class Flag: @unchecked Sendable {
    private var value = false
    private let lock = NSLock()
    func set() { lock.lock(); value = true; lock.unlock() }
    func get() -> Bool { lock.lock(); defer { lock.unlock() }; return value }
}

// Capability bits (LDICT_CAP_*).
private let capRead: UInt64 = 1 << 0
private let capInsert: UInt64 = 1 << 1
private let capRemove: UInt64 = 1 << 2
private let capClear: UInt64 = 1 << 3
private let capCompact: UInt64 = 1 << 4
private let capSubstring: UInt64 = 1 << 5
private let capCheckpoint: UInt64 = 1 << 6

private struct Fixture: Codable {
    struct Entry: Codable { let term: String; let value: UInt64? }
    struct ContainsCase: Codable { let term: String; let expected: Bool }
    struct GetCase: Codable { let term: String; let found: Bool; let value: UInt64? }
    struct FreqCase: Codable { let pattern: String; let expected: Int }
    struct SubCase: Codable { let pattern: String; let expected: Bool }
    let entries: [Entry]
    let size: Int
    let contains: [ContainsCase]
    let get: [GetCase]
    let substring_frequency: [FreqCase]
    let substring_contains: [SubCase]
}

final class ConformanceTests: XCTestCase {
    private static let fixture: Fixture = {
        let candidates =
            [ProcessInfo.processInfo.environment["LDICT_FIXTURE"]].compactMap { $0 }
            + ["../../canonical_fixture.json", "bindings/canonical_fixture.json",
               "../../../bindings/canonical_fixture.json"]
        let path = candidates.first { FileManager.default.fileExists(atPath: $0) } ?? candidates[0]
        let data = try! Data(contentsOf: URL(fileURLWithPath: path))
        return try! JSONDecoder().decode(Fixture.self, from: data)
    }()

    private var fixture: Fixture { Self.fixture }
    private func entries() -> [(String, UInt64?)] { fixture.entries.map { ($0.term, $0.value) } }

    private func assertFixtureReads(_ dictionary: Dictionary) throws {
        XCTAssertEqual(try dictionary.count, fixture.size)
        for item in fixture.contains {
            XCTAssertEqual(try dictionary.get(item.term).found, item.expected, item.term)
        }
        for item in fixture.get {
            let lookup = try dictionary.get(item.term)
            XCTAssertEqual(lookup.found, item.found, item.term)
            XCTAssertEqual(lookup.value, item.value, item.term)
        }
    }

    // C1 -------------------------------------------------------------------

    func testC1Identity() throws {
        XCTAssertEqual(Dictionary.abiVersion(), 1)
        XCTAssertEqual(Dictionary.apiRevision(), 5)
    }

    func testC1KindAndCapabilities() throws {
        let dawg = try DynamicDAWG()
        XCTAssertEqual(try dawg.kind, 1)
        let caps = try dawg.capabilities
        XCTAssertTrue(caps & capInsert != 0 && caps & capRemove != 0 && caps & capClear != 0 && caps & capCompact != 0)
        XCTAssertTrue(caps & capSubstring == 0 && caps & capCheckpoint == 0)
        dawg.close()
        let dat = try DoubleArrayTrie(entries: [("x", nil)])
        XCTAssertEqual(try dat.kind, 2)
        XCTAssertTrue(try dat.capabilities & capRead != 0)
        dat.close()
        let scdawg = try SCDAWG()
        XCTAssertEqual(try scdawg.kind, 3)
        XCTAssertTrue(try scdawg.capabilities & capSubstring != 0)
        scdawg.close()
    }

    // C2 -------------------------------------------------------------------

    func testC2DoubleCloseIdempotent() throws {
        let dawg = try DynamicDAWG()
        _ = try dawg.put("a")
        dawg.close()
        dawg.close() // idempotent
    }

    func testC2FreeOrderIndependence() throws {
        var dawgs: [DynamicDAWG] = []
        for i in 0..<4 {
            let d = try DynamicDAWG()
            _ = try d.put("term\(i)", value: UInt64(i))
            dawgs.append(d)
        }
        for index in [2, 0, 3, 1] { dawgs[index].close() }
    }

    // C3 -------------------------------------------------------------------

    func testC3DomainMismatch() throws {
        let dawg = try DynamicDAWG(unitDomain: .unicodeScalar)
        defer { dawg.close() }
        XCTAssertThrowsError(try dawg.put([1, 2])) { error in
            XCTAssertFalse((error as? LibdictensteinError)?.description.isEmpty ?? true)
        }
    }

    func testC3IoError() throws {
        XCTAssertThrowsError(try PersistentARTrie.open(at: "/nonexistent/ldict-swift-missing.part")) { error in
            XCTAssertFalse((error as? LibdictensteinError)?.description.isEmpty ?? true)
        }
    }

    // C4 -------------------------------------------------------------------

    func testC4DynamicDawg() throws {
        let dawg = try DynamicDAWG()
        defer { dawg.close() }
        for (term, value) in entries() { _ = try dawg.put(term, value: value) }
        try assertFixtureReads(dawg)
    }

    func testC4DoubleArrayTrie() throws {
        let dat = try DoubleArrayTrie(entries: entries())
        defer { dat.close() }
        try assertFixtureReads(dat)
    }

    func testC4PersistentArtrie() throws {
        let path = NSTemporaryDirectory() + "ldict-swift-c4-\(getpid()).part"
        try? FileManager.default.removeItem(atPath: path)
        let art = try PersistentARTrie.create(at: path)
        for (term, value) in entries() { _ = try art.put(term, value: value) }
        try assertFixtureReads(art)
        art.close()
        for suffix in ["", ".wal", ".wlock"] { try? FileManager.default.removeItem(atPath: path + suffix) }
    }

    func testC4Scdawg() throws {
        let scdawg = try SCDAWG()
        defer { scdawg.close() }
        for (term, value) in entries() { _ = try scdawg.put(term, value: value) }
        for item in fixture.substring_frequency {
            XCTAssertEqual(try scdawg.substringFrequency(item.pattern), item.expected, item.pattern)
        }
        for item in fixture.substring_contains {
            XCTAssertEqual(try scdawg.containsSubstring(item.pattern), item.expected, item.pattern)
        }
    }

    // C5 -------------------------------------------------------------------

    func testC5CrudRoundTrip() throws {
        let dawg = try DynamicDAWG()
        defer { dawg.close() }
        XCTAssertTrue(try dawg.put("cat", value: 1))
        XCTAssertFalse(try dawg.put("cat", value: 1)) // idempotent
        XCTAssertEqual(try dawg.get("cat").value, 1)
        XCTAssertTrue(try dawg.remove("cat"))
        XCTAssertFalse(try dawg.remove("cat"))
        XCTAssertFalse(try dawg.get("cat").found)
    }

    func testC5CompactPreservesTerms() throws {
        let dawg = try DynamicDAWG()
        defer { dawg.close() }
        for i in 0..<50 { _ = try dawg.put("t\(i)", value: UInt64(i)) }
        for i in stride(from: 0, to: 50, by: 2) { XCTAssertTrue(try dawg.remove("t\(i)")) }
        _ = try dawg.compact()
        XCTAssertEqual(try dawg.count, 25)
        XCTAssertEqual(try dawg.get("t1").value, 1)
        XCTAssertFalse(try dawg.get("t0").found)
    }

    func testC5SubstringUpdatesWithInserts() throws {
        let scdawg = try SCDAWG()
        defer { scdawg.close() }
        _ = try scdawg.put("cat", value: 1)
        _ = try scdawg.put("cot", value: 2)
        XCTAssertEqual(try scdawg.substringFrequency("t"), 2)
        XCTAssertTrue(try scdawg.put("cut"))
        XCTAssertEqual(try scdawg.substringFrequency("t"), 3)
    }

    func testC5CapabilityDerivedReject() throws {
        let dat = try DoubleArrayTrie(entries: [("x", nil)])
        defer { dat.close() }
        let caps = try dat.capabilities
        XCTAssertTrue(caps & (capInsert | capRemove | capClear | capCompact) == 0)
    }

    // C6 -------------------------------------------------------------------

    func testC6PrecomposedAndMultibyte() throws {
        let dawg = try DynamicDAWG()
        defer { dawg.close() }
        XCTAssertTrue(try dawg.put("caf\u{00E9}", value: 7)) // café, precomposed U+00E9
        XCTAssertTrue(try dawg.put("\u{1F980}", value: 255)) // crab, 4-byte scalar
        XCTAssertTrue(try dawg.get("caf\u{00E9}").found)
        XCTAssertEqual(try dawg.get("\u{1F980}").value, 255)
    }

    func testC6CombiningDistinctFromPrecomposed() throws {
        let dawg = try DynamicDAWG()
        defer { dawg.close() }
        let precomposed = "caf\u{00E9}"  // café, precomposed U+00E9
        let combining = "cafe\u{0301}"   // cafe + U+0301 combining acute
        XCTAssertTrue(try dawg.put(precomposed, value: 1))
        XCTAssertTrue(try dawg.put(combining, value: 2))
        XCTAssertEqual(try dawg.count, 2)
        XCTAssertEqual(try dawg.get(precomposed).value, 1)
        XCTAssertEqual(try dawg.get(combining).value, 2)
    }

    func testC6ByteDomainEmbeddedNul() throws {
        let dawg = try DynamicDAWG(unitDomain: .byte)
        defer { dawg.close() }
        let embeddedNul = "a\u{0}b" // encodes to bytes 0x61 0x00 0x62
        XCTAssertTrue(try dawg.put(embeddedNul, value: 1))
        XCTAssertTrue(try dawg.get(embeddedNul).found)
        XCTAssertEqual(try dawg.get(embeddedNul).value, 1)
    }

    func testC6U64ValuesZeroAndMax() throws {
        let dawg = try DynamicDAWG(unitDomain: .u64)
        defer { dawg.close() }
        XCTAssertTrue(try dawg.put([1, 2, 3], value: 0))
        XCTAssertTrue(try dawg.put([9], value: UInt64.max))
        XCTAssertEqual(try dawg.get([1, 2, 3]).value, 0)
        XCTAssertEqual(try dawg.get([9]).value, UInt64.max)
    }

    // C7 -------------------------------------------------------------------

    func testC7BatchSizes() throws {
        for size in [0, 1, 255, 256, 257, 1000] {
            let batch = (0..<size).map { ("t\($0)", UInt64($0) as UInt64?) }
            let dat = try DoubleArrayTrie(entries: batch)
            defer { dat.close() }
            XCTAssertEqual(try dat.count, size, "batch \(size) size")
            if size > 0 {
                XCTAssertEqual(try dat.get("t0").value, 0, "batch \(size) first")
                XCTAssertEqual(try dat.get("t\(size - 1)").value, UInt64(size - 1), "batch \(size) last")
            }
        }
    }

    func testC7EntrySnapshotPreservesDomainsValuesAndRevision() throws {
        let unicode = try DynamicDAWG()
        _ = try unicode.put("", value: nil)
        _ = try unicode.put("a", value: 0)
        _ = try unicode.put("é", value: UInt64.max)
        let stream = try unicode.entryStream(
            limits: EntryBatchLimits(maxEntries: 1, maxUnits: 8, maxValues: 1)
        )
        XCTAssertEqual(stream.info.unitDomain.cValue, UnitDomain.unicodeScalar.cValue)
        XCTAssertEqual(stream.info.exactCount, 3)
        _ = try unicode.put("later", value: 7)
        unicode.close()

        var captured: [DictionaryEntry] = []
        while let entry = try stream.next() { captured.append(entry) }
        XCTAssertEqual(captured.map(\.key.string), ["", "a", "é"])
        XCTAssertNil(captured[0].value)
        XCTAssertEqual(captured[1].value, 0)
        XCTAssertEqual(captured[2].value, UInt64.max)
        try stream.close()

        let bytes = try DynamicDAWG(unitDomain: .byte)
        defer { bytes.close() }
        let raw: [UInt8] = [0, 0xff]
        _ = try bytes.put(bytes: raw)
        let byteSnapshot = try bytes.entries()
        XCTAssertEqual(Array(byteSnapshot), [DictionaryEntry(key: .bytes(raw), value: nil)])

        let tokens = try DynamicDAWG(unitDomain: .u64)
        defer { tokens.close() }
        _ = try tokens.put([1, UInt64.max], value: 0)
        let tokenSnapshot = try tokens.entries()
        XCTAssertEqual(
            Array(tokenSnapshot),
            [DictionaryEntry(key: .u64([1, UInt64.max]), value: 0)]
        )
    }

    func testC7EntryStreamExplicitEarlyCancel() throws {
        let dictionary = try DynamicDAWG()
        defer { dictionary.close() }
        for term in ["a", "b", "c"] { _ = try dictionary.put(term) }
        let stream = try dictionary.entryStream(
            limits: EntryBatchLimits(maxEntries: 3, maxUnits: 3, maxValues: 0)
        )
        XCTAssertEqual(try stream.next()?.key.string, "a")
        try stream.cancel()
        XCTAssertNil(try stream.next())
        try stream.close()
        try stream.close()
    }

    // C8 -------------------------------------------------------------------

    func testC8CrudScriptMatchesOracle() throws {
        var rng = LCG(seed: 0xC0FFEE)
        let keys = (0..<40).map { "k\($0)" }
        var oracle: [String: UInt64?] = [:]
        let dawg = try DynamicDAWG()
        defer { dawg.close() }
        for _ in 0..<3000 {
            let key = keys[Int.random(in: 0..<keys.count, using: &rng)]
            let present = oracle.keys.contains(key)
            let op = Int.random(in: 0..<100, using: &rng)
            if op < 50 {
                let value: UInt64? = Int.random(in: 0..<2, using: &rng) == 0 ? nil : UInt64(Int.random(in: 0..<1_000_000_000, using: &rng))
                XCTAssertEqual(try dawg.put(key, value: value), !present)
                oracle[key] = value
            } else if op < 75 {
                XCTAssertEqual(try dawg.remove(key), present)
                oracle[key] = nil
            } else if op < 95 {
                XCTAssertEqual(try dawg.get(key).found, present)
                if present { XCTAssertEqual(try dawg.get(key).value, oracle[key] ?? nil) }
            } else {
                _ = try dawg.compact()
            }
            XCTAssertEqual(try dawg.count, oracle.count)
        }
    }

    func testC8SubstringMatchesNaiveOracle() throws {
        var rng = LCG(seed: 0x5CDA)
        let alphabet = Array("abcx")
        func generate(_ maxLen: Int) -> String {
            let n = Int.random(in: 1...maxLen, using: &rng)
            return String((0..<n).map { _ in alphabet[Int.random(in: 0..<alphabet.count, using: &rng)] })
        }
        var termsSet = Set<String>()
        while termsSet.count < 60 { termsSet.insert(generate(6)) }
        let terms = Array(termsSet)
        func naive(_ pattern: String) -> Int {
            var total = 0
            let patternChars = Array(pattern)
            for term in terms {
                let chars = Array(term)
                if chars.count < patternChars.count { continue }
                for start in 0...(chars.count - patternChars.count)
                where Array(chars[start..<start + patternChars.count]) == patternChars {
                    total += 1
                }
            }
            return total
        }
        let scdawg = try SCDAWG()
        defer { scdawg.close() }
        for term in terms { _ = try scdawg.put(term) }
        for _ in 0..<200 {
            let pattern = generate(3)
            let expected = naive(pattern)
            XCTAssertEqual(try scdawg.substringFrequency(pattern), expected, pattern)
            XCTAssertEqual(try scdawg.containsSubstring(pattern), expected > 0, pattern)
        }
    }

    // C9 -------------------------------------------------------------------

    private func rssKib() -> Int {
        guard let handle = FileHandle(forReadingAtPath: "/proc/self/status") else { return 0 }
        defer { try? handle.close() }
        let data = handle.readDataToEndOfFile()
        let content = String(decoding: data, as: UTF8.self)
        for line in content.split(separator: "\n") where line.hasPrefix("VmRSS:") {
            return Int(line.filter { $0.isNumber }) ?? 0
        }
        return 0
    }

    func testC9CreateUseFreeCyclesDoNotLeak() throws {
        let cycles = 12000
        for _ in 0..<2000 {
            let dawg = try DynamicDAWG()
            _ = try dawg.put("cat", value: 1)
            dawg.close()
        }
        let before = rssKib()
        for _ in 0..<cycles {
            let dawg = try DynamicDAWG()
            _ = try dawg.put("cat", value: 1)
            _ = try dawg.put("cot", value: 2)
            _ = try dawg.put("cut")
            XCTAssertTrue(try dawg.get("cot").found)
            dawg.close()
        }
        let after = rssKib()
        if before > 0 && after > before {
            XCTAssertLessThan(after - before, 64 * 1024, "RSS grew \(after - before) KiB over \(cycles) cycles")
        }
    }

    // C10 ------------------------------------------------------------------

    func testC10IndependentDictionariesPerTask() throws {
        let errors = AtomicInt()
        DispatchQueue.concurrentPerform(iterations: 8) { seed in
            do {
                let dawg = try DynamicDAWG()
                for i in 0..<2000 { _ = try dawg.put("t\(seed)_\(i)", value: UInt64(i)) }
                if try dawg.count != 2000 { errors.increment() }
                if try dawg.get("t\(seed)_1500").value != 1500 { errors.increment() }
                dawg.close()
            } catch { errors.increment() }
        }
        XCTAssertEqual(errors.current, 0)
    }

    func testC10ConcurrentReadersDuringWriter() throws {
        let dawg = try DynamicDAWG()
        defer { dawg.close() }
        for i in 0..<500 { _ = try dawg.put("seed\(i)", value: UInt64(i)) }
        let stop = Flag()
        let errors = AtomicInt()
        let group = DispatchGroup()
        for _ in 0..<4 {
            DispatchQueue.global().async(group: group) {
                while !stop.get() {
                    do {
                        if try !dawg.get("seed0").found { errors.increment() }
                        _ = try dawg.get("seed250")
                    } catch { errors.increment() }
                }
            }
        }
        for i in 500..<3000 { _ = try dawg.put("w\(i)", value: UInt64(i)) }
        stop.set()
        group.wait()
        XCTAssertEqual(errors.current, 0)
        XCTAssertEqual(try dawg.get("w2999").value, 2999)
    }
}

import Dispatch
import Foundation
import Libdictenstein

#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

private let defaultEntries = 65_536
private let defaultBatchSize = 256
private let defaultEarlyCancel = 64
private let keyUnits = 38

private enum ProfileArm: String, Codable {
    case materialized
    case stream
    case streamCancel = "stream-cancel"
}

private struct ProfileConfig {
    let arm: ProfileArm
    let entries: Int
    let passes: Int
    let warmupPasses: Int
    let batchSize: Int
    let earlyCancel: Int
}

private struct CorpusEntry {
    let key: [UInt8]
    let value: UInt64
}

private struct ProfileResult: Encodable {
    let schema: String
    let runtime: String
    let arm: ProfileArm
    let dictionaryEntries: Int
    let consumedEntriesPerPass: Int
    let passes: Int
    let warmupPasses: Int
    let batchSize: Int
    let earlyCancel: Int?
    let elapsedNS: UInt64
    let checksum: UInt64

    enum CodingKeys: String, CodingKey {
        case schema
        case runtime
        case arm
        case dictionaryEntries = "dictionary_entries"
        case consumedEntriesPerPass = "consumed_entries_per_pass"
        case passes
        case warmupPasses = "warmup_passes"
        case batchSize = "batch_size"
        case earlyCancel = "early_cancel"
        case elapsedNS = "elapsed_ns"
        case checksum
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(schema, forKey: .schema)
        try container.encode(runtime, forKey: .runtime)
        try container.encode(arm, forKey: .arm)
        try container.encode(dictionaryEntries, forKey: .dictionaryEntries)
        try container.encode(consumedEntriesPerPass, forKey: .consumedEntriesPerPass)
        try container.encode(passes, forKey: .passes)
        try container.encode(warmupPasses, forKey: .warmupPasses)
        try container.encode(batchSize, forKey: .batchSize)
        if let earlyCancel {
            try container.encode(earlyCancel, forKey: .earlyCancel)
        } else {
            try container.encodeNil(forKey: .earlyCancel)
        }
        try container.encode(elapsedNS, forKey: .elapsedNS)
        try container.encode(checksum, forKey: .checksum)
    }
}

private struct ProfileError: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = description
    }
}

private func parseInteger(_ value: String, option: String, allowZero: Bool = false) throws -> Int {
    guard let parsed = Int(value), allowZero ? parsed >= 0 : parsed > 0 else {
        throw ProfileError("\(option) must be \(allowZero ? "nonnegative" : "positive")")
    }
    return parsed
}

private func parseArguments(_ arguments: [String]) throws -> ProfileConfig {
    var arm: ProfileArm?
    var entries = defaultEntries
    var passes = 1
    var warmupPasses = 1
    var batchSize = defaultBatchSize
    var earlyCancel = defaultEarlyCancel
    var index = 0
    while index < arguments.count {
        let option = arguments[index]
        guard index + 1 < arguments.count else {
            throw ProfileError("missing value for \(option)")
        }
        let value = arguments[index + 1]
        switch option {
        case "--arm":
            guard let parsed = ProfileArm(rawValue: value) else {
                throw ProfileError("--arm must be materialized, stream, or stream-cancel")
            }
            arm = parsed
        case "--entries":
            entries = try parseInteger(value, option: option)
        case "--passes":
            passes = try parseInteger(value, option: option)
        case "--warmup-passes":
            warmupPasses = try parseInteger(value, option: option, allowZero: true)
        case "--batch-size":
            batchSize = try parseInteger(value, option: option)
        case "--early-cancel":
            earlyCancel = try parseInteger(value, option: option)
        default:
            throw ProfileError("unknown argument: \(option)")
        }
        index += 2
    }
    guard let arm else { throw ProfileError("--arm is required") }
    let (_, overflow) = batchSize.multipliedReportingOverflow(by: keyUnits)
    guard !overflow else { throw ProfileError("--batch-size is too large") }
    return ProfileConfig(
        arm: arm,
        entries: entries,
        passes: passes,
        warmupPasses: warmupPasses,
        batchSize: batchSize,
        earlyCancel: earlyCancel
    )
}

private func makeCorpus(size: Int) -> [CorpusEntry] {
    (0..<size).map { index in
        let term = String(
            format: "collection/%04x/%08x/shared-suffix",
            index & 0x0fff,
            index
        )
        return CorpusEntry(key: Array(term.utf8), value: UInt64(index))
    }
}

private func expectedChecksum(_ corpus: [CorpusEntry], limit: Int) -> UInt64 {
    let ordered = corpus.sorted { left, right in
        left.key.lexicographicallyPrecedes(right.key)
    }
    var checksum: UInt64 = 0
    for entry in ordered.prefix(limit) {
        checksum &+= UInt64(entry.key.count) ^ entry.value
    }
    return checksum
}

private func buildDictionary(_ corpus: [CorpusEntry]) throws -> DynamicDAWG {
    let dictionary = try DynamicDAWG(unitDomain: .byte)
    do {
        for entry in corpus {
            guard try dictionary.put(bytes: entry.key, value: entry.value) else {
                throw ProfileError("generated corpus contains a duplicate key")
            }
        }
        return dictionary
    } catch {
        dictionary.close()
        throw error
    }
}

private func checksum(_ entry: DictionaryEntry) throws -> UInt64 {
    guard case let .bytes(key) = entry.key else {
        throw ProfileError("benchmark expected a byte-domain entry")
    }
    return UInt64(key.count) ^ (entry.value ?? 0)
}

private func limits(batchSize: Int) -> EntryBatchLimits {
    EntryBatchLimits(
        maxEntries: batchSize,
        maxUnits: batchSize * keyUnits,
        maxValues: batchSize
    )
}

private func drainMaterialized(
    _ dictionary: Dictionary,
    batchSize: Int
) throws -> (checksum: UInt64, count: Int) {
    let snapshot = try dictionary.entries(limits: limits(batchSize: batchSize))
    var total: UInt64 = 0
    for entry in snapshot {
        total &+= try checksum(entry)
    }
    return (total, snapshot.count)
}

private func drainStream(
    _ dictionary: Dictionary,
    batchSize: Int,
    limit: Int,
    cancel: Bool
) throws -> (checksum: UInt64, count: Int) {
    let stream = try dictionary.entryStream(limits: limits(batchSize: batchSize))
    defer { try? stream.close() }
    var total: UInt64 = 0
    var processed = 0
    while processed < limit {
        guard let entry = try stream.next() else { break }
        total &+= try checksum(entry)
        processed += 1
    }
    if cancel {
        try stream.cancel()
    } else {
        guard processed == limit, try stream.next() == nil else {
            throw ProfileError("stream cardinality differs from the generated corpus")
        }
    }
    try stream.close()
    return (total, processed)
}

private func drain(
    _ dictionary: Dictionary,
    config: ProfileConfig
) throws -> (checksum: UInt64, count: Int) {
    switch config.arm {
    case .materialized:
        try drainMaterialized(dictionary, batchSize: config.batchSize)
    case .stream:
        try drainStream(
            dictionary,
            batchSize: config.batchSize,
            limit: config.entries,
            cancel: false
        )
    case .streamCancel:
        try drainStream(
            dictionary,
            batchSize: config.batchSize,
            limit: min(config.entries, config.earlyCancel),
            cancel: true
        )
    }
}

private func execute(arguments: [String]) throws {
    let config = try parseArguments(arguments)
    let corpus = makeCorpus(size: config.entries)
    let dictionary = try buildDictionary(corpus)
    defer { dictionary.close() }

    let consumed = config.arm == .streamCancel
        ? min(config.entries, config.earlyCancel)
        : config.entries
    let expected = expectedChecksum(corpus, limit: consumed)
    for _ in 0..<config.warmupPasses {
        let result = try drain(dictionary, config: config)
        guard result.count == consumed, result.checksum == expected else {
            throw ProfileError("warmup checksum or cardinality mismatch")
        }
    }

    let started = DispatchTime.now().uptimeNanoseconds
    var total: UInt64 = 0
    for _ in 0..<config.passes {
        let result = try drain(dictionary, config: config)
        guard result.count == consumed, result.checksum == expected else {
            throw ProfileError("timed checksum or cardinality mismatch")
        }
        total &+= result.checksum
    }
    let elapsed = DispatchTime.now().uptimeNanoseconds &- started
    guard total == expected &* UInt64(config.passes) else {
        throw ProfileError("aggregate checksum mismatch")
    }

    let result = ProfileResult(
        schema: "libdictenstein.host-collection-traversal.v1",
        runtime: "swift",
        arm: config.arm,
        dictionaryEntries: config.entries,
        consumedEntriesPerPass: consumed,
        passes: config.passes,
        warmupPasses: config.warmupPasses,
        batchSize: config.batchSize,
        earlyCancel: config.arm == .streamCancel ? config.earlyCancel : nil,
        elapsedNS: elapsed,
        checksum: total
    )
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    FileHandle.standardOutput.write(try encoder.encode(result))
    FileHandle.standardOutput.write(Data([0x0a]))
}

do {
    try execute(arguments: Array(CommandLine.arguments.dropFirst()))
} catch {
    FileHandle.standardError.write(Data("\(error)\n".utf8))
    exit(2)
}

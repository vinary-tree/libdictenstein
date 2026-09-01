import CLibdictenstein
import VinaryTreeInterop

public struct LibdictensteinError: Error, CustomStringConvertible, Sendable {
    public let description: String
    init(_ fallback: String) {
        let native = ldict_last_error_message().map(String.init(cString:)) ?? ""
        description = native.isEmpty ? fallback : native
    }
}

public struct Lookup: Sendable, Equatable {
    public let found: Bool
    public let value: UInt64?

    public init(found: Bool, value: UInt64?) {
        self.found = found
        self.value = value
    }
}

public struct EntryBatchLimits: Sendable, Equatable {
    public var maxEntries: Int
    public var maxUnits: Int
    public var maxValues: Int

    public init(maxEntries: Int = 256, maxUnits: Int = 4096, maxValues: Int = 256) {
        self.maxEntries = maxEntries
        self.maxUnits = maxUnits
        self.maxValues = maxValues
    }
}

public struct EntriesInfo: Sendable, Equatable {
    public let unitDomain: UnitDomain
    public let exactCount: Int?
    public let snapshotIdentity: (producer: UInt64, revision: UInt64)?

    public static func == (lhs: EntriesInfo, rhs: EntriesInfo) -> Bool {
        lhs.unitDomain.cValue == rhs.unitDomain.cValue
            && lhs.exactCount == rhs.exactCount
            && lhs.snapshotIdentity?.producer == rhs.snapshotIdentity?.producer
            && lhs.snapshotIdentity?.revision == rhs.snapshotIdentity?.revision
    }
}

public enum DictionaryEntryKey: Sendable, Equatable {
    case bytes([UInt8])
    case unicodeScalars([UInt32])
    case u64([UInt64])

    public var string: String? {
        guard case let .unicodeScalars(scalars) = self else { return nil }
        return String(String.UnicodeScalarView(scalars.compactMap(UnicodeScalar.init)))
    }
}

public struct DictionaryEntry: Sendable, Equatable {
    public let key: DictionaryEntryKey
    public let value: UInt64?
}

/// Host-owned materialization of one immutable native revision.
public struct EntrySnapshot: RandomAccessCollection, Sendable {
    public typealias Index = Int
    public typealias Element = DictionaryEntry

    public let info: EntriesInfo
    private let storage: [DictionaryEntry]

    init(info: EntriesInfo, entries: [DictionaryEntry]) {
        self.info = info
        storage = entries
    }

    public var startIndex: Int { storage.startIndex }
    public var endIndex: Int { storage.endIndex }
    public func index(after index: Int) -> Int { storage.index(after: index) }
    public func index(before index: Int) -> Int { storage.index(before: index) }
    public subscript(position: Int) -> DictionaryEntry { storage[position] }
}

private func checked(_ status: LdictStatus) throws {
    guard status == LDICT_STATUS_OK else {
        throw LibdictensteinError("libdictenstein status \(status.rawValue)")
    }
}

private func domain(_ value: UnitDomain) -> UInt32 {
    UInt32(value.cValue.rawValue)
}

/// Base owner for all project-defined dictionaries. Operations do not use a
/// facade-wide mutex; libdictenstein's native concurrency semantics are kept.
open class Dictionary: DictionaryResource, @unchecked Sendable {
    private var raw: OpaquePointer?
    public let unitDomain: UnitDomain

    init(raw: OpaquePointer, unitDomain: UnitDomain) {
        self.raw = raw
        self.unitDomain = unitDomain
    }

    deinit { close() }

    private func handle() throws -> OpaquePointer {
        guard let raw else { throw LibdictensteinError("dictionary is closed") }
        return raw
    }

    public func close() {
        if let raw {
            ldict_dictionary_free(raw)
            self.raw = nil
        }
    }

    public func withVtResource<Result>(
        _ body: (UnsafePointer<VtResource>) throws -> Result
    ) rethrows -> Result {
        var resource = VtResource()
        do { try checked(ldict_dictionary_resource(try handle(), &resource)) }
        catch { preconditionFailure("closed or incompatible dictionary: \(error)") }
        return try withUnsafePointer(to: &resource, body)
    }

    /// Native ABI version (LDICT_ABI_VERSION); always 1 for this family.
    public static func abiVersion() -> UInt32 { ldict_abi_version() }

    /// Compatible-additions revision within the ABI version (LDICT_API_REVISION).
    public static func apiRevision() -> UInt32 { ldict_api_revision() }

    /// Stable native backend identifier (LDICT_KIND_*).
    public var kind: Int {
        get throws {
            var value: UInt32 = 0
            try checked(ldict_dictionary_kind(try handle(), &value))
            return Int(value)
        }
    }

    /// Bitset of operations implemented by this backend (LDICT_CAP_*).
    public var capabilities: UInt64 {
        get throws {
            var value: UInt64 = 0
            try checked(ldict_dictionary_capabilities(try handle(), &value))
            return value
        }
    }

    public var count: Int {
        get throws {
            var result = 0
            try checked(ldict_dictionary_len(try handle(), &result))
            return result
        }
    }

    /// Capture a bounded single-pass entry stream at the current revision.
    public func entryStream(limits: EntryBatchLimits = EntryBatchLimits()) throws -> EntryStream {
        try EntryStream(dictionary: try handle(), limits: limits)
    }

    /// Materialize one immutable revision as a host-owned random-access collection.
    public func entries(limits: EntryBatchLimits = EntryBatchLimits()) throws -> EntrySnapshot {
        let stream = try entryStream(limits: limits)
        defer { stream.closeIgnoringErrors() }
        var values: [DictionaryEntry] = []
        if let exactCount = stream.info.exactCount {
            values.reserveCapacity(exactCount)
        }
        while let entry = try stream.next() {
            values.append(entry)
        }
        try stream.close()
        return EntrySnapshot(info: stream.info, entries: values)
    }

    @discardableResult
    public func put(_ term: String, value: UInt64? = nil) throws -> Bool {
        let bytes = Array(term.utf8)
        var inserted: UInt8 = 0
        try bytes.withUnsafeBufferPointer { buffer in
            try checked(ldict_dictionary_insert_text_value(
                try handle(), buffer.baseAddress, buffer.count,
                value ?? 0, value == nil ? 0 : 1, &inserted
            ))
        }
        return inserted != 0
    }

    @discardableResult
    public func put(_ term: [UInt64], value: UInt64? = nil) throws -> Bool {
        var inserted: UInt8 = 0
        try term.withUnsafeBufferPointer { buffer in
            try checked(ldict_dictionary_insert_u64_value(
                try handle(), buffer.baseAddress, buffer.count,
                value ?? 0, value == nil ? 0 : 1, &inserted
            ))
        }
        return inserted != 0
    }

    @discardableResult
    public func put(bytes term: [UInt8], value: UInt64? = nil) throws -> Bool {
        var inserted: UInt8 = 0
        try term.withUnsafeBufferPointer { buffer in
            try checked(ldict_dictionary_insert_text_value(
                try handle(), buffer.baseAddress, buffer.count,
                value ?? 0, value == nil ? 0 : 1, &inserted
            ))
        }
        return inserted != 0
    }

    @discardableResult
    public func remove(_ term: String) throws -> Bool {
        let bytes = Array(term.utf8)
        var removed: UInt8 = 0
        try bytes.withUnsafeBufferPointer { buffer in
            try checked(ldict_dictionary_remove_text(
                try handle(), buffer.baseAddress, buffer.count, &removed
            ))
        }
        return removed != 0
    }

    @discardableResult
    public func remove(_ term: [UInt64]) throws -> Bool {
        var removed: UInt8 = 0
        try term.withUnsafeBufferPointer { buffer in
            try checked(ldict_dictionary_remove_u64(
                try handle(), buffer.baseAddress, buffer.count, &removed
            ))
        }
        return removed != 0
    }

    @discardableResult
    public func remove(bytes term: [UInt8]) throws -> Bool {
        var removed: UInt8 = 0
        try term.withUnsafeBufferPointer { buffer in
            try checked(ldict_dictionary_remove_text(
                try handle(), buffer.baseAddress, buffer.count, &removed
            ))
        }
        return removed != 0
    }

    public func get(_ term: String) throws -> Lookup {
        let bytes = Array(term.utf8)
        var found: UInt8 = 0
        var hasValue: UInt8 = 0
        var value: UInt64 = 0
        try bytes.withUnsafeBufferPointer { buffer in
            try checked(ldict_dictionary_get_text_value(
                try handle(), buffer.baseAddress, buffer.count,
                &found, &value, &hasValue
            ))
        }
        return Lookup(found: found != 0, value: hasValue == 0 ? nil : value)
    }

    public func get(_ term: [UInt64]) throws -> Lookup {
        var found: UInt8 = 0
        var hasValue: UInt8 = 0
        var value: UInt64 = 0
        try term.withUnsafeBufferPointer { buffer in
            try checked(ldict_dictionary_get_u64_value(
                try handle(), buffer.baseAddress, buffer.count,
                &found, &value, &hasValue
            ))
        }
        return Lookup(found: found != 0, value: hasValue == 0 ? nil : value)
    }

    public func get(bytes term: [UInt8]) throws -> Lookup {
        var found: UInt8 = 0
        var hasValue: UInt8 = 0
        var value: UInt64 = 0
        try term.withUnsafeBufferPointer { buffer in
            try checked(ldict_dictionary_get_text_value(
                try handle(), buffer.baseAddress, buffer.count,
                &found, &value, &hasValue
            ))
        }
        return Lookup(found: found != 0, value: hasValue == 0 ? nil : value)
    }

    public func clear() throws { try checked(ldict_dictionary_clear(try handle())) }
    public func compact() throws -> Int {
        var reclaimed = 0
        try checked(ldict_dictionary_compact(try handle(), &reclaimed))
        return reclaimed
    }
    public func checkpoint() throws { try checked(ldict_dictionary_checkpoint(try handle())) }

    public func containsSubstring(_ pattern: String) throws -> Bool {
        let bytes = Array(pattern.utf8)
        var result: UInt8 = 0
        try bytes.withUnsafeBufferPointer { buffer in
            try checked(ldict_scdawg_contains_substring(
                try handle(), buffer.baseAddress, buffer.count, &result
            ))
        }
        return result != 0
    }

    public func substringFrequency(_ pattern: String) throws -> Int {
        let bytes = Array(pattern.utf8)
        var result = 0
        try bytes.withUnsafeBufferPointer { buffer in
            try checked(ldict_scdawg_substring_frequency(
                try handle(), buffer.baseAddress, buffer.count, &result
            ))
        }
        return result
    }
}

/// Throwing single-pass traversal over bounded native batches. Every returned
/// entry owns its key. Call close or cancel when exiting before exhaustion;
/// deinit is a final safety net.
public final class EntryStream: @unchecked Sendable {
    public private(set) var info: EntriesInfo

    private var cursor: OpaquePointer?
    private var limits: LdictEntryBatchLimits
    private var batch = LdictEntryBatch()
    private var index = 0
    private var leased = false
    private var ended = false

    fileprivate init(dictionary: OpaquePointer, limits: EntryBatchLimits) throws {
        guard limits.maxEntries > 0, limits.maxUnits >= 0, limits.maxValues >= 0 else {
            throw LibdictensteinError("entry batch limits are invalid")
        }
        self.limits = LdictEntryBatchLimits(
            max_entries: limits.maxEntries,
            max_units: limits.maxUnits,
            max_values: limits.maxValues,
            reserved: 0
        )
        info = EntriesInfo(unitDomain: .byte, exactCount: nil, snapshotIdentity: nil)
        var nativeInfo = LdictEntriesInfo()
        try checked(ldict_dictionary_entries_open(dictionary, &cursor, &nativeInfo))
        guard let unitDomain = Self.decodeDomain(nativeInfo.unit_domain) else {
            closeIgnoringErrors()
            throw LibdictensteinError("entry provider returned an unknown unit domain")
        }
        let exact = nativeInfo.flags & 1 == 0
            ? nil : nativeInfo.exact_len
        let identity = nativeInfo.flags & 2 == 0
            ? nil : (nativeInfo.identity.producer, nativeInfo.identity.revision)
        info = EntriesInfo(
            unitDomain: unitDomain,
            exactCount: exact,
            snapshotIdentity: identity
        )
    }

    deinit { closeIgnoringErrors() }

    private static func decodeDomain(_ raw: UInt32) -> UnitDomain? {
        switch raw {
        case domain(.byte): .byte
        case domain(.unicodeScalar): .unicodeScalar
        case domain(.u64): .u64
        default: nil
        }
    }

    private func checkedRange(offset: Int, length: Int, total: Int) throws -> Range<Int> {
        guard offset >= 0, length >= 0, offset <= total, length <= total - offset else {
            throw LibdictensteinError("entry provider returned an invalid arena range")
        }
        return offset..<(offset + length)
    }

    private func copyCurrent() throws -> DictionaryEntry {
        guard index >= 0, index < batch.entry_count, let descriptors = batch.entries else {
            throw LibdictensteinError("entry provider returned an invalid descriptor array")
        }
        let descriptor = descriptors[index]
        let range = try checkedRange(
            offset: descriptor.unit_offset,
            length: descriptor.unit_len,
            total: batch.unit_count
        )
        let key: DictionaryEntryKey
        switch info.unitDomain {
        case .byte:
            if range.isEmpty {
                key = .bytes([])
            } else {
                guard let units = batch.units else {
                    throw LibdictensteinError("entry byte arena is null")
                }
                let values = UnsafeBufferPointer(
                    start: units.assumingMemoryBound(to: UInt8.self),
                    count: batch.unit_count
                )
                key = .bytes(Array(values[range]))
            }
        case .unicodeScalar:
            if range.isEmpty {
                key = .unicodeScalars([])
            } else {
                guard let units = batch.units else {
                    throw LibdictensteinError("entry Unicode-scalar arena is null")
                }
                let values = UnsafeBufferPointer(
                    start: units.assumingMemoryBound(to: UInt32.self),
                    count: batch.unit_count
                )
                key = .unicodeScalars(Array(values[range]))
            }
        case .u64:
            if range.isEmpty {
                key = .u64([])
            } else {
                guard let units = batch.units else {
                    throw LibdictensteinError("entry u64 arena is null")
                }
                let values = UnsafeBufferPointer(
                    start: units.assumingMemoryBound(to: UInt64.self),
                    count: batch.unit_count
                )
                key = .u64(Array(values[range]))
            }
        }

        let value: UInt64?
        switch descriptor.value_len {
        case 0:
            value = nil
        case 1:
            let valueRange = try checkedRange(
                offset: descriptor.value_offset,
                length: 1,
                total: batch.value_count
            )
            guard let values = batch.values else {
                throw LibdictensteinError("entry value arena is null")
            }
            value = values[valueRange.lowerBound]
        default:
            throw LibdictensteinError("entry provider returned an invalid optional-u64 descriptor")
        }
        return DictionaryEntry(key: key, value: value)
    }

    private func releaseBatch() throws {
        guard leased else { return }
        try checked(ldict_entry_cursor_release(cursor, batch.generation))
        leased = false
        batch = LdictEntryBatch()
        index = 0
    }

    /// Return the next host-owned entry, or nil after sticky exhaustion.
    public func next() throws -> DictionaryEntry? {
        try withExtendedLifetime(self) {
            guard !ended, cursor != nil else { return nil }
            do {
                if !leased {
                    let status = ldict_entry_cursor_next(cursor, &limits, &batch)
                    if status == LDICT_STATUS_END {
                        ended = true
                        try close()
                        return nil
                    }
                    try checked(status)
                    leased = true
                    index = 0
                }
                let entry = try copyCurrent()
                index += 1
                if index == batch.entry_count {
                    try releaseBatch()
                }
                return entry
            } catch {
                closeIgnoringErrors()
                throw error
            }
        }
    }

    /// Request sticky early termination and settle any live batch lease.
    public func cancel() throws {
        try withExtendedLifetime(self) {
            guard cursor != nil, !ended else { return }
            try checked(ldict_entry_cursor_cancel(cursor))
            try releaseBatch()
            ended = true
        }
    }

    /// Deterministically cancel, release, and free the native cursor.
    public func close() throws {
        try withExtendedLifetime(self) {
            guard cursor != nil else { return }
            var firstError: Error?
            let cancelStatus = ldict_entry_cursor_cancel(cursor)
            if cancelStatus != LDICT_STATUS_OK {
                firstError = LibdictensteinError("entry cursor cancellation failed")
            }
            do { try releaseBatch() } catch { if firstError == nil { firstError = error } }
            let freeStatus = ldict_entry_cursor_free(cursor)
            if freeStatus == LDICT_STATUS_OK {
                cursor = nil
                ended = true
            } else if firstError == nil {
                firstError = LibdictensteinError("entry cursor close failed")
            }
            if let firstError { throw firstError }
        }
    }

    fileprivate func closeIgnoringErrors() {
        try? close()
    }
}

public final class DynamicDAWG: Dictionary, @unchecked Sendable {
    public init(unitDomain: UnitDomain = .unicodeScalar) throws {
        var raw: OpaquePointer?
        try checked(ldict_dynamic_dawg_new(domain(unitDomain), &raw))
        super.init(raw: raw!, unitDomain: unitDomain)
    }
}

public final class SCDAWG: Dictionary, @unchecked Sendable {
    public init(unitDomain: UnitDomain = .unicodeScalar) throws {
        var raw: OpaquePointer?
        try checked(ldict_scdawg_new(domain(unitDomain), &raw))
        super.init(raw: raw!, unitDomain: unitDomain)
    }
}

public final class DoubleArrayTrie: Dictionary, @unchecked Sendable {
    public init(entries: [(String, UInt64?)], unitDomain: UnitDomain = .unicodeScalar) throws {
        let encoded = entries.map { Array($0.0.utf8) }
        let offsets = encoded.reduce(into: [Int]()) { values, term in
            values.append((values.last ?? 0) + (values.count == 0 ? 0 : encoded[values.count - 1].count))
        }
        let flat = encoded.flatMap { $0 }
        var descriptors = entries.enumerated().map { index, entry in
            LdictTextEntry(
                data: nil,
                len: encoded[index].count,
                value: LdictOptionalU64(
                    value: entry.1 ?? 0,
                    has_value: entry.1 == nil ? 0 : 1,
                    reserved: (0, 0, 0, 0, 0, 0, 0)
                )
            )
        }
        var raw: OpaquePointer?
        try flat.withUnsafeBufferPointer { buffer in
            for index in descriptors.indices {
                descriptors[index].data = buffer.baseAddress?.advanced(by: offsets[index])
            }
            try descriptors.withUnsafeBufferPointer { values in
                try checked(ldict_double_array_trie_new(
                    domain(unitDomain), values.baseAddress, values.count, &raw
                ))
            }
        }
        super.init(raw: raw!, unitDomain: unitDomain)
    }
}

public final class PersistentARTrie: Dictionary, @unchecked Sendable {
    public static func create(at path: String, unitDomain: UnitDomain = .unicodeScalar) throws -> PersistentARTrie {
        try construct(path, unitDomain, ldict_persistent_artrie_create)
    }
    public static func open(at path: String, unitDomain: UnitDomain = .unicodeScalar) throws -> PersistentARTrie {
        try construct(path, unitDomain, ldict_persistent_artrie_open)
    }
    private static func construct(
        _ path: String,
        _ unitDomain: UnitDomain,
        _ constructor: (UInt32, UnsafePointer<UInt8>?, Int, UnsafeMutablePointer<OpaquePointer?>?) -> LdictStatus
    ) throws -> PersistentARTrie {
        let bytes = Array(path.utf8)
        var raw: OpaquePointer?
        try bytes.withUnsafeBufferPointer { buffer in
            try checked(constructor(domain(unitDomain), buffer.baseAddress, buffer.count, &raw))
        }
        return PersistentARTrie(raw: raw!, unitDomain: unitDomain)
    }
}

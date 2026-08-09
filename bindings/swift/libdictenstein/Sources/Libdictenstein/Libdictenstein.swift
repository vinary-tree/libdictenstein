import CLibdictenstein
import VinaryTreeInterop

public struct LibdictensteinError: Error, CustomStringConvertible, Sendable {
    public let description: String
    init(_ fallback: String) {
        description = ldict_last_error_message().map(String.init(cString:)) ?? fallback
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

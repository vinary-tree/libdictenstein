"""An immutable revision supplied by a Julia dictionary implementation."""
abstract type AbstractHostDictionarySnapshot end

"""Capture one immutable revision. A snapshot returns itself in constant time."""
provider_capture(snapshot::AbstractHostDictionarySnapshot) = snapshot

"""The key-unit domain of a captured revision."""
function provider_unit_domain end

"""The value domain of a captured revision (unit or optional UInt64)."""
function provider_value_domain end

"""The root node identifier of a captured revision."""
function provider_root end

"""Whether a node terminates a stored key."""
function provider_isfinal end

"""The mapped UInt64 of a final node, or `nothing` when absent."""
provider_value(::AbstractHostDictionarySnapshot, ::UInt64) = nothing

"""The exact key count when cheaply available, otherwise `nothing`."""
provider_length(::AbstractHostDictionarySnapshot) = nothing

function checked_provider_length(snapshot::AbstractHostDictionarySnapshot)
    count = provider_length(snapshot)
    count === nothing && return nothing
    count isa Integer && 0 <= count <= typemax(Int) ||
        throw(ArgumentError("provider length must fit a nonnegative Julia Int"))
    Int(count)
end

"""Return one bounded edge page and the total degree of a node."""
function provider_edge_page end

"""Follow one edge. Implementations may specialize this direct lookup."""
function provider_transition(snapshot::AbstractHostDictionarySnapshot,
    node::UInt64, label::UInt64)
    start = 0
    previous = nothing
    domain = UInt32(provider_unit_domain(snapshot))
    while true
        page, total = provider_edge_page(snapshot, node, start, 256)
        total isa Integer && 0 <= total <= typemax(Int) &&
            start <= total && length(page) <= min(256, total - start) ||
            throw(ArgumentError("edge provider returned an invalid page"))
        for edge in page
            edge isa VTI.VtDictionaryEdge || throw(ArgumentError(
                "edge provider returned a non-ABI edge"))
            previous !== nothing && edge.label <= previous &&
                throw(ArgumentError("node edges are not strictly ordered"))
            if domain == UInt32(VTI.UNIT_BYTE)
                edge.label <= typemax(UInt8) || throw(ArgumentError(
                    "byte edge label is invalid"))
            elseif domain == UInt32(VTI.UNIT_UNICODE_SCALAR)
                edge.label <= 0x10ffff &&
                    !(0xd800 <= edge.label <= 0xdfff) ||
                    throw(ArgumentError("Unicode edge label is invalid"))
            end
            edge.label == label && return edge.node
            previous = edge.label
        end
        start += length(page)
        start == total && return nothing
        isempty(page) && throw(ArgumentError("edge provider made no progress"))
    end
end

"""Fused finality and bounded edge page; defaults to the base operations."""
provider_visit(snapshot::AbstractHostDictionarySnapshot, node::UInt64,
    start::Int, capacity::Int) =
    (provider_isfinal(snapshot, node),
        provider_edge_page(snapshot, node, start, capacity)...)

"""Optional immutable process-local producer/revision identity."""
provider_identity(::AbstractHostDictionarySnapshot) = nothing

"""Optional compact immutable graph, described by `HostDictionaryGraph`."""
provider_graph(::AbstractHostDictionarySnapshot) = nothing

"""Value of a nonzero opaque graph cursor; defaults to a trie node lookup."""
provider_graph_value(snapshot::AbstractHostDictionarySnapshot,
    cursor::UInt64) = provider_value(snapshot, cursor)

"""Optional fresh finite lexicographic iterator of key/value pairs."""
provider_entries(::AbstractHostDictionarySnapshot) = nothing

"""An optional compact graph with dense node indexes and nonzero value cursors."""
struct HostDictionaryGraph
    nodes::Vector{VTI.VtDictionaryGraphNode}
    edges::Vector{VTI.VtDictionaryGraphEdge}
    root::UInt64
end

struct HostGraphMemory
    nodes::Ptr{VTI.VtDictionaryGraphNode}
    node_count::Int
    edges::Ptr{VTI.VtDictionaryGraphEdge}
    edge_count::Int
    root::UInt64
end

function malloc_host_copy(items::Vector{T}) where {T}
    isempty(items) && return Ptr{T}(C_NULL)
    bytes = Base.Checked.checked_mul(length(items), sizeof(T))
    output = Ptr{T}(Base.Libc.malloc(bytes))
    output == C_NULL && throw(OutOfMemoryError())
    GC.@preserve items unsafe_copyto!(output, pointer(items), length(items))
    output
end

function copy_host_graph(graph::HostDictionaryGraph, unit_domain::UInt32,
    max_nodes::Int, max_edges::Int)
    isempty(graph.nodes) && throw(ArgumentError("a compact graph needs a root node"))
    length(graph.nodes) <= max_nodes && length(graph.edges) <= max_edges ||
        throw(ArgumentError("compact graph exceeds its node or edge budget"))
    graph.root < length(graph.nodes) || throw(ArgumentError(
        "compact graph root is outside the node arena"))
    for node in graph.nodes
        node.edge_start <= length(graph.edges) &&
            node.edge_len <= length(graph.edges) - node.edge_start &&
            node.is_final <= 1 && node.value_cursor != 0 &&
            all(iszero, node.reserved) ||
            throw(ArgumentError("compact graph node is malformed"))
    end
    for edge in graph.edges
        edge.target < length(graph.nodes) || throw(ArgumentError(
            "compact graph edge has an invalid target"))
        if unit_domain == UInt32(VTI.UNIT_BYTE)
            edge.label <= typemax(UInt8) || throw(ArgumentError(
                "compact graph byte label is invalid"))
        elseif unit_domain == UInt32(VTI.UNIT_UNICODE_SCALAR)
            edge.label <= 0x10ffff && !(0xd800 <= edge.label <= 0xdfff) ||
                throw(ArgumentError("compact graph Unicode label is invalid"))
        end
    end
    for node in graph.nodes
        previous = nothing
        first = Int(node.edge_start) + 1
        last = Int(node.edge_start + node.edge_len)
        for index in first:last
            label = graph.edges[index].label
            previous !== nothing && label <= previous &&
                throw(ArgumentError("compact graph edges are not ordered"))
            previous = label
        end
    end
    nodes = malloc_host_copy(graph.nodes)
    edges = Ptr{VTI.VtDictionaryGraphEdge}(C_NULL)
    try
        edges = malloc_host_copy(graph.edges)
        HostGraphMemory(nodes, length(graph.nodes), edges, length(graph.edges),
            graph.root)
    catch
        Base.Libc.free(nodes)
        edges == C_NULL || Base.Libc.free(edges)
        rethrow()
    end
end

mutable struct HostEntryLease
    entries::Ptr{VTI.VtDictionaryEntryRaw}
    entry_count::Int
    units::Ptr{Cvoid}
    unit_count::Int
    values::Ptr{UInt64}
    value_count::Int
    generation::UInt64
end

function free_host_lease(lease::HostEntryLease)
    Base.Libc.free(lease.entries)
    lease.units == C_NULL || Base.Libc.free(lease.units)
    lease.values == C_NULL || Base.Libc.free(lease.values)
    nothing
end

mutable struct HostEntriesCursor
    context::Ptr{Cvoid}
    iterator::Any
    iterator_state::Any
    started::Bool
    pending::Any
    previous::Any
    exhausted::Bool
    cancelled::Bool
    generation::UInt64
    lease::Union{Nothing,HostEntryLease}
    lock::ReentrantLock
end

const HOST_CURSOR_REGISTRY = Dict{Ptr{Cvoid},HostEntriesCursor}()
const HOST_CURSOR_REGISTRY_LOCK = ReentrantLock()
const HOST_CURSOR_NEXT_TOKEN = Ref{UInt}(1)

function host_cursor_call(body,
    pointer::Ptr{VTI.VtDictionaryEntriesCursorRaw})::Cint
    pointer == C_NULL && return Cint(VTI.STATUS_NULL_POINTER)
    raw = unsafe_load(pointer)
    raw.context == C_NULL && return Cint(VTI.STATUS_CLOSED)
    cursor = lock(HOST_CURSOR_REGISTRY_LOCK) do
        get(HOST_CURSOR_REGISTRY, raw.context, nothing)
    end
    cursor === nothing && return Cint(VTI.STATUS_CLOSED)
    lock(cursor.lock) do
        try
            body(cursor)
        catch error
            cursor.cancelled = true
            try
                state = host_dictionary_state(cursor.context)
                host_dictionary_failure!(state, error)
            catch
            end
            Cint(VTI.STATUS_PROVIDER_ERROR)
        end
    end
end

function host_key_units(key, domain::UInt32, max_units::Int)
    if domain == UInt32(VTI.UNIT_BYTE)
        if key isa AbstractString
            ncodeunits(key) <= max_units || throw(ArgumentError(
                "entry key exceeds the provider's unit budget"))
            return Vector{UInt8}(codeunits(key))
        end
        if key isa AbstractVector{UInt8}
            length(key) <= max_units || throw(ArgumentError(
                "entry key exceeds the provider's unit budget"))
            return Vector{UInt8}(key)
        end
    elseif domain == UInt32(VTI.UNIT_UNICODE_SCALAR)
        if key isa AbstractString
            length(key) <= max_units || throw(ArgumentError(
                "entry key exceeds the provider's unit budget"))
            return UInt32[UInt32(char) for char in key]
        end
        if key isa AbstractVector{UInt32}
            length(key) <= max_units || throw(ArgumentError(
                "entry key exceeds the provider's unit budget"))
            return Vector{UInt32}(key)
        end
    elseif domain == UInt32(VTI.UNIT_U64)
        if key isa AbstractVector{UInt64}
            length(key) <= max_units || throw(ArgumentError(
                "entry key exceeds the provider's unit budget"))
            return Vector{UInt64}(key)
        end
    end
    throw(ArgumentError("entry key is incompatible with its unit domain"))
end

function host_key_isless(left, right)
    for (a, b) in zip(left, right)
        a < b && return true
        a > b && return false
    end
    length(left) < length(right)
end

function host_next_entry!(cursor::HostEntriesCursor, provider)
    if cursor.pending !== nothing
        item = cursor.pending
        cursor.pending = nothing
        return item
    end
    step = cursor.started ? iterate(cursor.iterator, cursor.iterator_state) :
        iterate(cursor.iterator)
    cursor.started = true
    if step === nothing
        cursor.exhausted = true
        return nothing
    end
    entry, cursor.iterator_state = step
    key, value = if entry isa Pair
        (first(entry), last(entry))
    elseif entry isa VTI.DictionaryEntry
        (entry.units, entry.value)
    else
        throw(ArgumentError("entry iterator must yield Pair or DictionaryEntry"))
    end
    units = host_key_units(key, provider.unit_domain,
        provider.max_key_units)
    if provider.unit_domain == UInt32(VTI.UNIT_UNICODE_SCALAR)
        all(unit -> unit <= 0x10ffff && !(0xd800 <= unit <= 0xdfff),
            units) || throw(ArgumentError("entry key contains an invalid scalar"))
    end
    cursor.previous !== nothing && !host_key_isless(cursor.previous, units) &&
        throw(ArgumentError("entry keys must be strictly lexicographic"))
    cursor.previous = units
    if provider.value_domain == UInt32(VTI.VALUE_UNIT)
        value === nothing || throw(ArgumentError(
            "unit-valued entry has a mapped value"))
    else
        value === nothing || value isa UInt64 || throw(ArgumentError(
            "mapped entry value must be UInt64 or nothing"))
    end
    (units, value)
end

function host_entry_batch!(cursor::HostEntriesCursor, provider,
    limits::VTI.BatchLimits)
    cursor.lease === nothing || return Cint(VTI.STATUS_BATCH_IN_USE)
    cursor.cancelled && return Cint(VTI.STATUS_END)
    limits.max_entries > 0 && limits.reserved == 0 ||
        return Cint(VTI.STATUS_INVALID_ARGUMENT)
    descriptors = VTI.VtDictionaryEntryRaw[]
    units = provider.unit_domain == UInt32(VTI.UNIT_BYTE) ? UInt8[] :
        provider.unit_domain == UInt32(VTI.UNIT_UNICODE_SCALAR) ? UInt32[] :
        UInt64[]
    values = UInt64[]
    while length(descriptors) < limits.max_entries
        item = host_next_entry!(cursor, provider)
        item === nothing && break
        key, value = item
        needed_units = Base.Checked.checked_add(length(units), length(key))
        needed_values = Base.Checked.checked_add(length(values),
            value === nothing ? 0 : 1)
        if needed_units > limits.max_units || needed_values > limits.max_values
            cursor.pending = item
            isempty(descriptors) && return Cint(VTI.STATUS_LIMIT_EXCEEDED)
            break
        end
        push!(descriptors, VTI.VtDictionaryEntryRaw(Csize_t(length(units)),
            Csize_t(length(key)), Csize_t(length(values)),
            Csize_t(value === nothing ? 0 : 1), 0))
        append!(units, key)
        value === nothing || push!(values, value)
    end
    isempty(descriptors) && return Cint(VTI.STATUS_END)
    next_generation = Base.Checked.checked_add(cursor.generation, 1)
    entries_pointer = malloc_host_copy(descriptors)
    units_pointer = Ptr{Cvoid}(C_NULL)
    values_pointer = Ptr{UInt64}(C_NULL)
    try
        units_pointer = Ptr{Cvoid}(malloc_host_copy(units))
        values_pointer = malloc_host_copy(values)
        cursor.generation = next_generation
        cursor.lease = HostEntryLease(entries_pointer, length(descriptors),
            units_pointer, length(units), values_pointer, length(values),
            next_generation)
        Cint(VTI.STATUS_OK)
    catch
        Base.Libc.free(entries_pointer)
        units_pointer == C_NULL || Base.Libc.free(units_pointer)
        values_pointer == C_NULL || Base.Libc.free(values_pointer)
        rethrow()
    end
end

function host_entry_view(lease::HostEntryLease)
    VTI.VtDictionaryEntryBatchView(lease.entries, Csize_t(lease.entry_count),
        lease.units, Csize_t(lease.unit_count), lease.values,
        Csize_t(lease.value_count), lease.generation, 0)
end

function host_release_lease!(cursor::HostEntriesCursor, generation::UInt64)
    lease = cursor.lease
    lease === nothing && return Cint(VTI.STATUS_INVALID_ARGUMENT)
    lease.generation == generation || return Cint(VTI.STATUS_INVALID_ARGUMENT)
    free_host_lease(lease)
    cursor.lease = nothing
    Cint(VTI.STATUS_OK)
end

function free_host_graph(graph::HostGraphMemory)
    Base.Libc.free(graph.nodes)
    graph.edges == C_NULL || Base.Libc.free(graph.edges)
    nothing
end

mutable struct HostDictionaryState
    references::Int
    snapshot::AbstractHostDictionarySnapshot
    unit_domain::UInt32
    value_domain::UInt32
    identity::Union{Nothing,VTI.SnapshotIdentity}
    graph::Union{Nothing,HostGraphMemory}
    has_entries::Bool
    max_key_units::Int
    lock::ReentrantLock
    last_error::String
    dictionary_table::Base.RefValue{VTI.VtDictionaryVTable}
end

const HOST_DICTIONARY_REGISTRY = Dict{Ptr{Cvoid},HostDictionaryState}()
const HOST_DICTIONARY_REGISTRY_LOCK = ReentrantLock()
const HOST_DICTIONARY_NEXT_TOKEN = Ref{UInt}(1)

function host_dictionary_state(context::Ptr{Cvoid})
    context == C_NULL && throw(ArgumentError("null Julia dictionary context"))
    state = lock(HOST_DICTIONARY_REGISTRY_LOCK) do
        get(HOST_DICTIONARY_REGISTRY, context, nothing)
    end
    state === nothing && throw(ArgumentError("closed Julia dictionary context"))
    state
end

function host_dictionary_failure!(state::HostDictionaryState, error)
    state.last_error = sprint(showerror, error)
    nothing
end

function host_dictionary_call(body, context::Ptr{Cvoid})::Cint
    state = try
        host_dictionary_state(context)
    catch
        return Cint(VTI.STATUS_CLOSED)
    end
    lock(state.lock) do
        try
            body(state)
        catch error
            host_dictionary_failure!(state, error)
            Cint(VTI.STATUS_PROVIDER_ERROR)
        end
    end
end

function host_dictionary_retain_checked(context::Ptr{Cvoid})
    lock(HOST_DICTIONARY_REGISTRY_LOCK) do
        state = get(HOST_DICTIONARY_REGISTRY, context, nothing)
        state === nothing && return false
        state.references == typemax(Int) && return false
        state.references += 1
        true
    end
end

function host_dictionary_retain(context::Ptr{Cvoid})::Cvoid
    try
        host_dictionary_retain_checked(context)
    catch
        # The ABI's retain callback cannot report a failure or unwind into C.
    end
    nothing
end

function host_dictionary_release(context::Ptr{Cvoid})::Cvoid
    try
        lock(HOST_DICTIONARY_REGISTRY_LOCK) do
            state = HOST_DICTIONARY_REGISTRY[context]
            state.references -= 1
            if state.references == 0
                delete!(HOST_DICTIONARY_REGISTRY, context)
                state.graph === nothing || free_host_graph(state.graph)
            end
        end
    catch
        # A stale release cannot be reported through this void ABI callback.
    end
    nothing
end

const HOST_DICTIONARY_RETAIN = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_RELEASE = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_QUERY = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_SNAPSHOT = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_ROOT = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_LEN = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_ISFINAL = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_VALUE = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_TRANSITION = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_EDGES = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_VISIT = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_IDENTITY = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_GRAPH = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_GRAPH_VALUE = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_ENTRIES_OPEN = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_ENTRIES_NEXT = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_ENTRIES_RELEASE = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_ENTRIES_REDUCE = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_ENTRIES_CANCEL = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_ENTRIES_CLOSE = Ref{Ptr{Cvoid}}(C_NULL)
const HOST_DICTIONARY_RESOURCE_TABLE = Ref{VTI.VtResourceVTable}()
const HOST_DICTIONARY_VISIT_TABLE = Ref{VTI.VtDictionaryVisitVTable}()
const HOST_DICTIONARY_IDENTITY_TABLE = Ref{VTI.VtSnapshotIdentityVTable}()
const HOST_DICTIONARY_GRAPH_TABLE = Ref{VTI.VtDictionaryGraphVTable}()
const HOST_DICTIONARY_ENTRIES_TABLE = Ref{VTI.VtDictionaryEntriesVTable}()

function host_dictionary_query(context::Ptr{Cvoid},
    requested::Ptr{VTI.VtInterfaceId}, minimum::UInt32,
    output::Ptr{Ptr{Cvoid}})::Cint
    (requested == C_NULL || output == C_NULL) && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        id = unsafe_load(requested)
        table = if id == VTI.DICTIONARY_INTERFACE_ID &&
            minimum <= VTI.DICTIONARY_INTERFACE_VERSION
            Base.unsafe_convert(Ptr{VTI.VtDictionaryVTable},
                state.dictionary_table)
        elseif id == VTI.DICTIONARY_VISIT_INTERFACE_ID &&
            minimum <= VTI.DICTIONARY_VISIT_INTERFACE_VERSION
            Base.unsafe_convert(Ptr{VTI.VtDictionaryVisitVTable},
                HOST_DICTIONARY_VISIT_TABLE)
        elseif id == VTI.SNAPSHOT_IDENTITY_INTERFACE_ID &&
            minimum <= VTI.SNAPSHOT_IDENTITY_INTERFACE_VERSION &&
            state.identity !== nothing
            Base.unsafe_convert(Ptr{VTI.VtSnapshotIdentityVTable},
                HOST_DICTIONARY_IDENTITY_TABLE)
        elseif id == VTI.DICTIONARY_GRAPH_INTERFACE_ID &&
            minimum <= VTI.DICTIONARY_GRAPH_INTERFACE_VERSION &&
            state.graph !== nothing
            Base.unsafe_convert(Ptr{VTI.VtDictionaryGraphVTable},
                HOST_DICTIONARY_GRAPH_TABLE)
        elseif id == VTI.DICTIONARY_ENTRIES_INTERFACE_ID &&
            minimum <= VTI.DICTIONARY_ENTRIES_INTERFACE_VERSION &&
            state.has_entries
            Base.unsafe_convert(Ptr{VTI.VtDictionaryEntriesVTable},
                HOST_DICTIONARY_ENTRIES_TABLE)
        else
            C_NULL
        end
        unsafe_store!(output, Ptr{Cvoid}(table))
        table == C_NULL ? Cint(VTI.STATUS_UNSUPPORTED) : Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_snapshot(context::Ptr{Cvoid},
    output::Ptr{VTI.VtResourceRaw})::Cint
    output == C_NULL && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do _
        host_dictionary_retain_checked(context) ||
            return Cint(VTI.STATUS_PROVIDER_ERROR)
        unsafe_store!(output, VTI.VtResourceRaw(context,
            Base.unsafe_convert(Ptr{VTI.VtResourceVTable},
                HOST_DICTIONARY_RESOURCE_TABLE)))
        Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_root(context::Ptr{Cvoid}, output::Ptr{UInt64})::Cint
    output == C_NULL && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        unsafe_store!(output, UInt64(provider_root(state.snapshot)))
        Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_len(context::Ptr{Cvoid}, output::Ptr{Csize_t},
    known::Ptr{UInt8})::Cint
    (output == C_NULL || known == C_NULL) && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        count = checked_provider_length(state.snapshot)
        count === nothing ? (unsafe_store!(output, Csize_t(0));
            unsafe_store!(known, UInt8(0))) :
            (unsafe_store!(output, Csize_t(count));
                unsafe_store!(known, UInt8(1)))
        Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_isfinal(context::Ptr{Cvoid}, node::UInt64,
    output::Ptr{UInt8})::Cint
    output == C_NULL && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        unsafe_store!(output, UInt8(Bool(provider_isfinal(state.snapshot, node))))
        Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_value(context::Ptr{Cvoid}, node::UInt64,
    output::Ptr{VTI.VtOptionalU64})::Cint
    output == C_NULL && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        value = provider_value(state.snapshot, node)
        state.value_domain == UInt32(VTI.VALUE_UNIT) && value !== nothing &&
            throw(ArgumentError("unit-valued dictionary returned a mapped value"))
        optional = VTI.VtOptionalU64(value === nothing ? 0 : UInt64(value),
            UInt8(value !== nothing), ntuple(_ -> UInt8(0), 7))
        unsafe_store!(output, optional)
        Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_graph_value(context::Ptr{Cvoid}, cursor::UInt64,
    output::Ptr{VTI.VtOptionalU64})::Cint
    output == C_NULL && return Cint(VTI.STATUS_NULL_POINTER)
    cursor == 0 && return Cint(VTI.STATUS_INVALID_ARGUMENT)
    host_dictionary_call(context) do state
        value = provider_graph_value(state.snapshot, cursor)
        state.value_domain == UInt32(VTI.VALUE_UNIT) && value !== nothing &&
            throw(ArgumentError("unit-valued graph returned a mapped value"))
        optional = VTI.VtOptionalU64(value === nothing ? 0 : UInt64(value),
            UInt8(value !== nothing), ntuple(_ -> UInt8(0), 7))
        unsafe_store!(output, optional)
        Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_transition(context::Ptr{Cvoid}, node::UInt64,
    label::UInt64, output::Ptr{UInt64}, found::Ptr{UInt8})::Cint
    (output == C_NULL || found == C_NULL) && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        child = provider_transition(state.snapshot, node, label)
        unsafe_store!(output, child === nothing ? UInt64(0) : UInt64(child))
        unsafe_store!(found, UInt8(child !== nothing))
        Cint(VTI.STATUS_OK)
    end
end

function validate_host_edge_page(state::HostDictionaryState, page,
    total, start::Csize_t, capacity::Csize_t)
    start <= typemax(Int) && capacity <= typemax(Int) ||
        throw(ArgumentError("edge page exceeds Julia index capacity"))
    0 <= total <= typemax(Int) || throw(ArgumentError("invalid node degree"))
    Int(start) <= total || throw(ArgumentError("edge offset exceeds node degree"))
    length(page) <= capacity && length(page) <= total - Int(start) ||
        throw(ArgumentError("edge page exceeds its declared bounds"))
    isempty(page) && Int(start) < total && capacity != 0 &&
        throw(ArgumentError("edge provider made no progress"))
    previous = nothing
    for edge in page
        edge isa VTI.VtDictionaryEdge || throw(ArgumentError(
            "edge page contains a non-ABI edge"))
        if state.unit_domain == UInt32(VTI.UNIT_BYTE)
            edge.label <= typemax(UInt8) || throw(ArgumentError("byte label is invalid"))
        elseif state.unit_domain == UInt32(VTI.UNIT_UNICODE_SCALAR)
            edge.label <= 0x10ffff && !(0xd800 <= edge.label <= 0xdfff) ||
                throw(ArgumentError("Unicode scalar label is invalid"))
        end
        previous !== nothing && edge.label <= previous &&
            throw(ArgumentError("node edges are not strictly ordered"))
        previous = edge.label
    end
    (page, total)
end

function checked_host_edges(state::HostDictionaryState, node::UInt64,
    start::Csize_t, capacity::Csize_t)
    page, total = provider_edge_page(state.snapshot, node,
        Int(start), Int(capacity))
    validate_host_edge_page(state, page, total, start, capacity)
end

function publish_host_edges(page, total, output::Ptr{VTI.VtDictionaryEdge},
    written::Ptr{Csize_t}, reported_total::Ptr{Csize_t})
    for (index, edge) in enumerate(page)
        unsafe_store!(output, edge, index)
    end
    unsafe_store!(written, Csize_t(length(page)))
    unsafe_store!(reported_total, Csize_t(total))
    Cint(VTI.STATUS_OK)
end

function host_dictionary_edges(context::Ptr{Cvoid}, node::UInt64,
    start::Csize_t, output::Ptr{VTI.VtDictionaryEdge}, capacity::Csize_t,
    written::Ptr{Csize_t}, total::Ptr{Csize_t})::Cint
    (written == C_NULL || total == C_NULL ||
        (capacity != 0 && output == C_NULL)) && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        page, degree = checked_host_edges(state, node, start, capacity)
        publish_host_edges(page, degree, output, written, total)
    end
end

function host_dictionary_visit(context::Ptr{Cvoid}, node::UInt64,
    start::Csize_t, finality::Ptr{UInt8},
    output::Ptr{VTI.VtDictionaryEdge}, capacity::Csize_t,
    written::Ptr{Csize_t}, total::Ptr{Csize_t})::Cint
    (finality == C_NULL || written == C_NULL || total == C_NULL ||
        (capacity != 0 && output == C_NULL)) && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        final, page, degree = provider_visit(state.snapshot, node,
            Int(start), Int(capacity))
        validate_host_edge_page(state, page, degree, start, capacity)
        unsafe_store!(finality, UInt8(Bool(final)))
        publish_host_edges(page, degree, output, written, total)
    end
end

function host_dictionary_graph(context::Ptr{Cvoid},
    output::Ptr{VTI.VtDictionaryGraphView})::Cint
    output == C_NULL && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        graph = state.graph
        graph === nothing && return Cint(VTI.STATUS_UNSUPPORTED)
        unsafe_store!(output, VTI.VtDictionaryGraphView(graph.nodes,
            Csize_t(graph.node_count), graph.edges, Csize_t(graph.edge_count),
            graph.root, 0))
        Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_entries_open(context::Ptr{Cvoid},
    output::Ptr{VTI.VtDictionaryEntriesCursorRaw},
    info::Ptr{VTI.VtDictionaryEntriesInfo})::Cint
    (output == C_NULL || info == C_NULL) && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        iterator = provider_entries(state.snapshot)
        iterator === nothing && return Cint(VTI.STATUS_UNSUPPORTED)
        count = checked_provider_length(state.snapshot)
        flags = (count === nothing ? UInt64(0) :
            VTI.ENTRIES_INFO_FLAG_EXACT_LEN) |
            (state.identity === nothing ? UInt64(0) :
                VTI.ENTRIES_INFO_FLAG_SNAPSHOT_IDENTITY)
        identity = state.identity === nothing ? VTI.SnapshotIdentity(0, 0) :
            state.identity
        host_dictionary_retain_checked(context) ||
            return Cint(VTI.STATUS_PROVIDER_ERROR)
        cursor = HostEntriesCursor(context, iterator, nothing, false, nothing,
            nothing, false, false, 0, nothing, ReentrantLock())
        token = try
            lock(HOST_CURSOR_REGISTRY_LOCK) do
                next = HOST_CURSOR_NEXT_TOKEN[]
                HOST_CURSOR_NEXT_TOKEN[] = Base.Checked.checked_add(next, 1)
                pointer = Ptr{Cvoid}(next)
                HOST_CURSOR_REGISTRY[pointer] = cursor
                pointer
            end
        catch
            host_dictionary_release(context)
            rethrow()
        end
        unsafe_store!(info, VTI.VtDictionaryEntriesInfo(state.unit_domain,
            state.value_domain, UInt32(VTI.ENTRY_LEXICOGRAPHIC), 0,
            flags, Csize_t(count === nothing ? 0 : count), identity, (0, 0)))
        unsafe_store!(output, VTI.VtDictionaryEntriesCursorRaw(token,
            Base.unsafe_convert(Ptr{VTI.VtDictionaryEntriesVTable},
                HOST_DICTIONARY_ENTRIES_TABLE)))
        Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_entries_next(
    pointer::Ptr{VTI.VtDictionaryEntriesCursorRaw},
    limits::Ptr{VTI.BatchLimits},
    output::Ptr{VTI.VtDictionaryEntryBatchView})::Cint
    (limits == C_NULL || output == C_NULL) && return Cint(VTI.STATUS_NULL_POINTER)
    host_cursor_call(pointer) do cursor
        state = host_dictionary_state(cursor.context)
        status = host_entry_batch!(cursor, state, unsafe_load(limits))
        status == Cint(VTI.STATUS_OK) &&
            unsafe_store!(output, host_entry_view(cursor.lease))
        status
    end
end

function host_dictionary_entries_release(
    pointer::Ptr{VTI.VtDictionaryEntriesCursorRaw},
    generation::UInt64)::Cint
    host_cursor_call(pointer) do cursor
        host_release_lease!(cursor, generation)
    end
end

function host_dictionary_entries_reduce(
    pointer::Ptr{VTI.VtDictionaryEntriesCursorRaw},
    limits::Ptr{VTI.BatchLimits}, reducer::Ptr{Cvoid},
    reducer_context::Ptr{Cvoid}, out_count::Ptr{Csize_t})::Cint
    (limits == C_NULL || reducer == C_NULL || out_count == C_NULL) &&
        return Cint(VTI.STATUS_NULL_POINTER)
    host_cursor_call(pointer) do cursor
        state = host_dictionary_state(cursor.context)
        count = 0
        while true
            status = host_entry_batch!(cursor, state, unsafe_load(limits))
            if status == Cint(VTI.STATUS_END)
                unsafe_store!(out_count, Csize_t(count))
                return Cint(VTI.STATUS_OK)
            end
            status == Cint(VTI.STATUS_OK) || return status
            lease = cursor.lease
            page = Ref(host_entry_view(lease))
            callback_status = try
                ccall(reducer, Cint,
                    (Ptr{Cvoid}, Ref{VTI.VtDictionaryEntryBatchView}),
                    reducer_context, page)
            finally
                host_release_lease!(cursor, lease.generation)
            end
            count = Base.Checked.checked_add(count, lease.entry_count)
            if callback_status == Cint(VTI.STATUS_END)
                unsafe_store!(out_count, Csize_t(count))
                return Cint(VTI.STATUS_OK)
            end
            callback_status == Cint(VTI.STATUS_OK) || return callback_status
            cursor.cancelled && (unsafe_store!(out_count, Csize_t(count));
                return Cint(VTI.STATUS_OK))
        end
    end
end

function host_dictionary_entries_cancel(
    pointer::Ptr{VTI.VtDictionaryEntriesCursorRaw})::Cint
    host_cursor_call(pointer) do cursor
        cursor.cancelled = true
        Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_entries_close(
    pointer::Ptr{VTI.VtDictionaryEntriesCursorRaw})::Cint
    host_cursor_call(pointer) do cursor
        cursor.lease === nothing || return Cint(VTI.STATUS_BATCH_IN_USE)
        token = unsafe_load(pointer).context
        lock(HOST_CURSOR_REGISTRY_LOCK) do
            delete!(HOST_CURSOR_REGISTRY, token)
        end
        unsafe_store!(pointer, VTI.VtDictionaryEntriesCursorRaw(C_NULL, C_NULL))
        host_dictionary_release(cursor.context)
        Cint(VTI.STATUS_OK)
    end
end

function host_dictionary_identity(context::Ptr{Cvoid},
    output::Ptr{VTI.SnapshotIdentity})::Cint
    output == C_NULL && return Cint(VTI.STATUS_NULL_POINTER)
    host_dictionary_call(context) do state
        state.identity === nothing && return Cint(VTI.STATUS_UNSUPPORTED)
        unsafe_store!(output, state.identity)
        Cint(VTI.STATUS_OK)
    end
end

"""Capture a customer Julia dictionary and expose its immutable VTI resource."""
function host_dictionary(source; max_graph_nodes::Integer=1_000_000,
    max_graph_edges::Integer=8_000_000,
    max_key_units::Integer=1_000_000)
    0 < max_graph_nodes <= typemax(Int) &&
        0 <= max_graph_edges <= typemax(Int) &&
        0 <= max_key_units <= typemax(Int) ||
        throw(ArgumentError("invalid host dictionary resource limits"))
    snapshot = provider_capture(source)
    snapshot isa AbstractHostDictionarySnapshot || throw(ArgumentError(
        "provider_capture must return an AbstractHostDictionarySnapshot"))
    unit = UInt32(provider_unit_domain(snapshot))
    unit in (UInt32(VTI.UNIT_BYTE), UInt32(VTI.UNIT_UNICODE_SCALAR),
        UInt32(VTI.UNIT_U64)) || throw(ArgumentError("unsupported key-unit domain"))
    domain = UInt32(provider_value_domain(snapshot))
    domain in (UInt32(VTI.VALUE_UNIT), UInt32(VTI.VALUE_OPTIONAL_U64)) ||
        throw(ArgumentError("this provider requires unit or optional-u64 values"))
    UInt64(provider_root(snapshot))
    checked_provider_length(snapshot)
    identity = provider_identity(snapshot)
    identity === nothing || identity isa VTI.SnapshotIdentity ||
        throw(ArgumentError("provider identity has the wrong ABI type"))
    graph_spec = provider_graph(snapshot)
    graph_spec === nothing || graph_spec isa HostDictionaryGraph ||
        throw(ArgumentError("provider graph has the wrong ABI type"))
    entries = provider_entries(snapshot)
    graph = graph_spec === nothing ? nothing :
        copy_host_graph(graph_spec, unit, Int(max_graph_nodes),
            Int(max_graph_edges))
    context = Ptr{Cvoid}(C_NULL)
    transferred = false
    try
        table = Ref{VTI.VtDictionaryVTable}()
        state = HostDictionaryState(1, snapshot, unit, domain, identity, graph,
            entries !== nothing, Int(max_key_units), ReentrantLock(), "", table)
        table[] = VTI.VtDictionaryVTable(sizeof(VTI.VtDictionaryVTable),
            VTI.DICTIONARY_INTERFACE_VERSION, unit, domain,
            VTI.DICTIONARY_FLAG_IMMUTABLE, HOST_DICTIONARY_SNAPSHOT[],
            HOST_DICTIONARY_ROOT[], HOST_DICTIONARY_LEN[],
            HOST_DICTIONARY_ISFINAL[], HOST_DICTIONARY_VALUE[],
            HOST_DICTIONARY_TRANSITION[], HOST_DICTIONARY_EDGES[])
        context = lock(HOST_DICTIONARY_REGISTRY_LOCK) do
            token = HOST_DICTIONARY_NEXT_TOKEN[]
            HOST_DICTIONARY_NEXT_TOKEN[] = Base.Checked.checked_add(token, 1)
            pointer = Ptr{Cvoid}(token)
            HOST_DICTIONARY_REGISTRY[pointer] = state
            pointer
        end
        raw = VTI.VtResourceRaw(context,
            Base.unsafe_convert(Ptr{VTI.VtResourceVTable},
                HOST_DICTIONARY_RESOURCE_TABLE))
        resource = VTI.adopt_resource(raw)
        transferred = true
        VTI.dictionary(resource; take=true)
    catch
        if !transferred
            context == C_NULL || lock(HOST_DICTIONARY_REGISTRY_LOCK) do
                delete!(HOST_DICTIONARY_REGISTRY, context)
            end
            graph === nothing || free_host_graph(graph)
        end
        rethrow()
    end
end

function init_host_dictionary_callbacks!()
    HOST_DICTIONARY_RETAIN[] = @cfunction(host_dictionary_retain, Cvoid,
        (Ptr{Cvoid},))
    HOST_DICTIONARY_RELEASE[] = @cfunction(host_dictionary_release, Cvoid,
        (Ptr{Cvoid},))
    HOST_DICTIONARY_QUERY[] = @cfunction(host_dictionary_query, Cint,
        (Ptr{Cvoid}, Ptr{VTI.VtInterfaceId}, UInt32, Ptr{Ptr{Cvoid}}))
    HOST_DICTIONARY_SNAPSHOT[] = @cfunction(host_dictionary_snapshot, Cint,
        (Ptr{Cvoid}, Ptr{VTI.VtResourceRaw}))
    HOST_DICTIONARY_ROOT[] = @cfunction(host_dictionary_root, Cint,
        (Ptr{Cvoid}, Ptr{UInt64}))
    HOST_DICTIONARY_LEN[] = @cfunction(host_dictionary_len, Cint,
        (Ptr{Cvoid}, Ptr{Csize_t}, Ptr{UInt8}))
    HOST_DICTIONARY_ISFINAL[] = @cfunction(host_dictionary_isfinal, Cint,
        (Ptr{Cvoid}, UInt64, Ptr{UInt8}))
    HOST_DICTIONARY_VALUE[] = @cfunction(host_dictionary_value, Cint,
        (Ptr{Cvoid}, UInt64, Ptr{VTI.VtOptionalU64}))
    HOST_DICTIONARY_TRANSITION[] = @cfunction(host_dictionary_transition, Cint,
        (Ptr{Cvoid}, UInt64, UInt64, Ptr{UInt64}, Ptr{UInt8}))
    HOST_DICTIONARY_EDGES[] = @cfunction(host_dictionary_edges, Cint,
        (Ptr{Cvoid}, UInt64, Csize_t, Ptr{VTI.VtDictionaryEdge}, Csize_t,
            Ptr{Csize_t}, Ptr{Csize_t}))
    HOST_DICTIONARY_VISIT[] = @cfunction(host_dictionary_visit, Cint,
        (Ptr{Cvoid}, UInt64, Csize_t, Ptr{UInt8},
            Ptr{VTI.VtDictionaryEdge}, Csize_t, Ptr{Csize_t}, Ptr{Csize_t}))
    HOST_DICTIONARY_IDENTITY[] = @cfunction(host_dictionary_identity, Cint,
        (Ptr{Cvoid}, Ptr{VTI.SnapshotIdentity}))
    HOST_DICTIONARY_GRAPH[] = @cfunction(host_dictionary_graph, Cint,
        (Ptr{Cvoid}, Ptr{VTI.VtDictionaryGraphView}))
    HOST_DICTIONARY_GRAPH_VALUE[] = @cfunction(host_dictionary_graph_value,
        Cint, (Ptr{Cvoid}, UInt64, Ptr{VTI.VtOptionalU64}))
    HOST_DICTIONARY_ENTRIES_OPEN[] = @cfunction(host_dictionary_entries_open,
        Cint, (Ptr{Cvoid}, Ptr{VTI.VtDictionaryEntriesCursorRaw},
            Ptr{VTI.VtDictionaryEntriesInfo}))
    HOST_DICTIONARY_ENTRIES_NEXT[] = @cfunction(host_dictionary_entries_next,
        Cint, (Ptr{VTI.VtDictionaryEntriesCursorRaw}, Ptr{VTI.BatchLimits},
            Ptr{VTI.VtDictionaryEntryBatchView}))
    HOST_DICTIONARY_ENTRIES_RELEASE[] = @cfunction(
        host_dictionary_entries_release, Cint,
        (Ptr{VTI.VtDictionaryEntriesCursorRaw}, UInt64))
    HOST_DICTIONARY_ENTRIES_REDUCE[] = @cfunction(
        host_dictionary_entries_reduce, Cint,
        (Ptr{VTI.VtDictionaryEntriesCursorRaw}, Ptr{VTI.BatchLimits},
            Ptr{Cvoid}, Ptr{Cvoid}, Ptr{Csize_t}))
    HOST_DICTIONARY_ENTRIES_CANCEL[] = @cfunction(
        host_dictionary_entries_cancel, Cint,
        (Ptr{VTI.VtDictionaryEntriesCursorRaw},))
    HOST_DICTIONARY_ENTRIES_CLOSE[] = @cfunction(
        host_dictionary_entries_close, Cint,
        (Ptr{VTI.VtDictionaryEntriesCursorRaw},))
    HOST_DICTIONARY_RESOURCE_TABLE[] = VTI.VtResourceVTable(
        sizeof(VTI.VtResourceVTable), VTI.ABI_VERSION, 0,
        HOST_DICTIONARY_RETAIN[], HOST_DICTIONARY_RELEASE[],
        HOST_DICTIONARY_QUERY[])
    HOST_DICTIONARY_VISIT_TABLE[] = VTI.VtDictionaryVisitVTable(
        sizeof(VTI.VtDictionaryVisitVTable),
        VTI.DICTIONARY_VISIT_INTERFACE_VERSION, 0,
        HOST_DICTIONARY_VISIT[])
    HOST_DICTIONARY_IDENTITY_TABLE[] = VTI.VtSnapshotIdentityVTable(
        sizeof(VTI.VtSnapshotIdentityVTable),
        VTI.SNAPSHOT_IDENTITY_INTERFACE_VERSION, 0,
        HOST_DICTIONARY_IDENTITY[])
    HOST_DICTIONARY_GRAPH_TABLE[] = VTI.VtDictionaryGraphVTable(
        sizeof(VTI.VtDictionaryGraphVTable),
        VTI.DICTIONARY_GRAPH_INTERFACE_VERSION, 0,
        HOST_DICTIONARY_GRAPH[], HOST_DICTIONARY_GRAPH_VALUE[])
    HOST_DICTIONARY_ENTRIES_TABLE[] = VTI.VtDictionaryEntriesVTable(
        sizeof(VTI.VtDictionaryEntriesVTable),
        VTI.DICTIONARY_ENTRIES_INTERFACE_VERSION, 0,
        HOST_DICTIONARY_ENTRIES_OPEN[], HOST_DICTIONARY_ENTRIES_NEXT[],
        HOST_DICTIONARY_ENTRIES_RELEASE[], HOST_DICTIONARY_ENTRIES_REDUCE[],
        HOST_DICTIONARY_ENTRIES_CANCEL[], HOST_DICTIONARY_ENTRIES_CLOSE[])
    nothing
end

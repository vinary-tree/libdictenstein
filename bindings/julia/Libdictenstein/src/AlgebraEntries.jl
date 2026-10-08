"""A single-use, snapshot-pinned, bounded native dictionary entry stream.

`algebra_entries` combines two dictionaries lazily; `prefix_entries` reads one
dictionary. Both use the native iterative entry cursor. Call `close` when
stopping early; exhaustion closes the cursor automatically.
"""
mutable struct AlgebraEntries{K}
    handle::Ptr{Cvoid}
    domain::VTI.UnitDomain
    limits::VTI.BatchLimits
    prefix::Union{Nothing,K}
    closed::Bool
    exhausted::Bool
end

function AlgebraEntries(handle::Ptr{Cvoid}, domain::VTI.UnitDomain,
    limits::VTI.BatchLimits, prefix)
    handle == C_NULL && throw(ArgumentError("native entry cursor is null"))
    K = key_type(domain)
    stream = AlgebraEntries{K}(handle, domain, limits, prefix, false, false)
    finalizer(close!, stream)
    stream
end

function close!(stream::AlgebraEntries)
    stream.closed && return nothing
    checked(abi_ldict_entry_cursor_free(stream.handle), :ldict_entry_cursor_free)
    stream.handle = C_NULL
    stream.closed = true
    nothing
end

Base.close(stream::AlgebraEntries) = close!(stream)
Base.isopen(stream::AlgebraEntries) = !stream.closed
Base.IteratorEltype(::Type{<:AlgebraEntries}) = Base.HasEltype()
Base.IteratorSize(::Type{<:AlgebraEntries}) = Base.SizeUnknown()
Base.eltype(::Type{AlgebraEntries{K}}) where {K} = Pair{K,Union{Nothing,UInt64}}

function normalize_prefix(domain::VTI.UnitDomain, prefix)
    prefix === nothing && return nothing
    domain == UNIT_UNICODE_SCALAR && prefix isa AbstractString && return String(prefix)
    domain == UNIT_BYTE && return text_buffer(prefix, domain)
    domain == UNIT_U64 && return u64_buffer(prefix, domain)
    throw(ArgumentError("prefix does not match dictionary unit domain"))
end

function validate_stream_info(info::VTI.VtDictionaryEntriesInfo, domain::VTI.UnitDomain)
    info.unit_domain == UInt32(domain) &&
        info.value_domain == UInt32(VTI.VALUE_OPTIONAL_U64) &&
        info.order == UInt32(VTI.ENTRY_LEXICOGRAPHIC) &&
        info.reserved0 == 0 && all(iszero, info.reserved) ||
        throw(NativeError(STATUS_PROVIDER_ERROR, :entry_cursor_open,
            "native cursor metadata disagrees with its dictionary domain"))
    nothing
end

function empty_entries_info()
    VTI.VtDictionaryEntriesInfo(0, 0, 0, 0, 0, 0,
        VTI.SnapshotIdentity(0, 0), (0, 0))
end

"""Open lazy set or valued algebra over two immutable captured revisions.

The result uses bounded pages of at most `page_size` records and `max_units`
native units. `value_merge` applies only to keys present in both inputs. An
optional prefix filters the resulting ordered stream without materializing it.
"""
function algebra_entries(left::Dictionary, right::Dictionary;
    operation::AlgebraOperation=ALGEBRA_UNION,
    value_merge::ValueMerge=VALUE_MERGE_LAST,
    prefix=nothing,
    page_size::Integer=256,
    max_units::Integer=65_536)
    require_revision9(:ldict_dictionary_algebra_entries_open)
    left.domain == right.domain || throw(ArgumentError("dictionary domains must match"))
    normalized_prefix = normalize_prefix(left.domain, prefix)
    limits = VTI.BatchLimits(page_size, max_units, page_size)
    cursor = Ref{Ptr{Cvoid}}(C_NULL)
    info = Ref(empty_entries_info())
    checked(abi_ldict_dictionary_algebra_entries_open(require_open(left),
        require_open(right), UInt32(operation), UInt32(value_merge), cursor, info),
        :ldict_dictionary_algebra_entries_open)
    try
        validate_stream_info(info[], left.domain)
        AlgebraEntries(cursor[], left.domain, limits, normalized_prefix)
    catch
        cursor[] == C_NULL || abi_ldict_entry_cursor_free(cursor[])
        rethrow()
    end
end

"""Open a lazy native dictionary traversal restricted to one key prefix."""
function prefix_entries(dictionary::Dictionary, prefix;
    page_size::Integer=256, max_units::Integer=65_536)
    normalized_prefix = normalize_prefix(dictionary.domain, prefix)
    limits = VTI.BatchLimits(page_size, max_units, page_size)
    cursor = Ref{Ptr{Cvoid}}(C_NULL)
    info = Ref(empty_entries_info())
    checked(abi_ldict_dictionary_entries_open(require_open(dictionary), cursor, info),
        :ldict_dictionary_entries_open)
    try
        validate_stream_info(info[], dictionary.domain)
        AlgebraEntries(cursor[], dictionary.domain, limits, normalized_prefix)
    catch
        cursor[] == C_NULL || abi_ldict_entry_cursor_free(cursor[])
        rethrow()
    end
end

function copy_key(batch::VTI.VtDictionaryEntryBatchView,
    descriptor::VTI.VtDictionaryEntryRaw, domain::VTI.UnitDomain)
    descriptor.unit_offset <= batch.unit_count &&
        descriptor.unit_len <= batch.unit_count - descriptor.unit_offset ||
        throw(NativeError(STATUS_PROVIDER_ERROR, :ldict_entry_cursor_next,
            "entry unit range exceeds the native page"))
    count = Int(descriptor.unit_len)
    count == 0 && return domain == UNIT_UNICODE_SCALAR ? "" :
        domain == UNIT_BYTE ? UInt8[] : UInt64[]
    batch.units != C_NULL || throw(NativeError(STATUS_PROVIDER_ERROR,
        :ldict_entry_cursor_next, "nonempty native page has no units"))
    offset = Int(descriptor.unit_offset)
    if domain == UNIT_BYTE
        arena = unsafe_wrap(Vector{UInt8}, Ptr{UInt8}(batch.units),
            Int(batch.unit_count); own=false)
        return copy(@view arena[offset+1:offset+count])
    elseif domain == UNIT_UNICODE_SCALAR
        arena = unsafe_wrap(Vector{UInt32}, Ptr{UInt32}(batch.units),
            Int(batch.unit_count); own=false)
        scalars = copy(@view arena[offset+1:offset+count])
        all(isvalid(Char, scalar) for scalar in scalars) ||
            throw(NativeError(STATUS_PROVIDER_ERROR, :ldict_entry_cursor_next,
                "native entry contains an invalid Unicode scalar"))
        return String(Char.(scalars))
    else
        arena = unsafe_wrap(Vector{UInt64}, Ptr{UInt64}(batch.units),
            Int(batch.unit_count); own=false)
        return copy(@view arena[offset+1:offset+count])
    end
end

function copy_entry(batch::VTI.VtDictionaryEntryBatchView,
    descriptor::VTI.VtDictionaryEntryRaw, domain::VTI.UnitDomain)
    descriptor.reserved == 0 && descriptor.value_len <= 1 &&
        descriptor.value_offset <= batch.value_count &&
        descriptor.value_len <= batch.value_count - descriptor.value_offset ||
        throw(NativeError(STATUS_PROVIDER_ERROR, :ldict_entry_cursor_next,
            "entry value range exceeds the native page"))
    key = copy_key(batch, descriptor, domain)
    value = if descriptor.value_len == 0
        nothing
    else
        batch.values != C_NULL || throw(NativeError(STATUS_PROVIDER_ERROR,
            :ldict_entry_cursor_next, "nonempty native page has no values"))
        unsafe_load(batch.values, Int(descriptor.value_offset) + 1)
    end
    key => value
end

prefix_matches(key::AbstractString, prefix::AbstractString) = startswith(key, prefix)
function prefix_matches(key::AbstractVector, prefix::AbstractVector)
    length(key) >= length(prefix) || return false
    all(key[index] == prefix[index] for index in eachindex(prefix))
end

lex_less(left::AbstractString, right::AbstractString) = isless(left, right)
function lex_less(left::AbstractVector, right::AbstractVector)
    for index in 1:min(length(left), length(right))
        left[index] < right[index] && return true
        left[index] > right[index] && return false
    end
    length(left) < length(right)
end

function empty_entry_batch()
    VTI.VtDictionaryEntryBatchView(Ptr{VTI.VtDictionaryEntryRaw}(C_NULL), 0,
        Ptr{Cvoid}(C_NULL), 0, Ptr{UInt64}(C_NULL), 0, 0, 0)
end

"""Copy and release the next bounded lexicographic page; return `nothing` at end."""
function next_page!(stream::AlgebraEntries{K}) where {K}
    stream.exhausted && return nothing
    stream.closed && throw(NativeError(STATUS_CLOSED, :entry_cursor,
        "entry cursor is closed"))
    while true
        batch = Ref(empty_entry_batch())
        status = abi_ldict_entry_cursor_next(stream.handle, Ref(stream.limits), batch)
        if status == Cint(STATUS_END)
            stream.exhausted = true
            close!(stream)
            return nothing
        end
        checked(status, :ldict_entry_cursor_next)
        page = Pair{K,Union{Nothing,UInt64}}[]
        beyond_prefix = false
        try
            raw = batch[]
            raw.entries != C_NULL && raw.entry_count > 0 ||
                throw(NativeError(STATUS_PROVIDER_ERROR, :ldict_entry_cursor_next,
                    "native cursor returned an empty page"))
            for index in 1:Int(raw.entry_count)
                entry = copy_entry(raw, unsafe_load(raw.entries, index), stream.domain)
                if stream.prefix === nothing || prefix_matches(first(entry), stream.prefix)
                    push!(page, entry)
                elseif !lex_less(first(entry), stream.prefix)
                    beyond_prefix = true
                    break
                end
            end
        finally
            checked(abi_ldict_entry_cursor_release(stream.handle,
                batch[].generation), :ldict_entry_cursor_release)
        end
        if beyond_prefix
            stream.exhausted = true
            close!(stream)
        end
        !isempty(page) && return page
        stream.exhausted && return nothing
    end
end

function Base.iterate(stream::AlgebraEntries{K}, state=nothing) where {K}
    page, index = state === nothing ?
        (Pair{K,Union{Nothing,UInt64}}[], 1) : state
    while index > length(page)
        page = next_page!(stream)
        page === nothing && return nothing
        index = 1
    end
    (page[index], (page, index + 1))
end

mutable struct EntryReductionState
    f::Any
    accumulator::Any
    stream::AlgebraEntries
    failed::Bool
    failure::Any
end

function reduce_entries_callback(context::Ptr{Cvoid},
    batch_pointer::Ptr{VTI.VtDictionaryEntryBatchView})::UInt32
    state = unsafe_pointer_to_objref(context)::EntryReductionState
    try
        batch_pointer != C_NULL || throw(NativeError(STATUS_PROVIDER_ERROR,
            :ldict_entry_cursor_reduce, "native reducer passed a null page"))
        batch = unsafe_load(batch_pointer)
        batch.entries != C_NULL && batch.entry_count > 0 ||
            throw(NativeError(STATUS_PROVIDER_ERROR, :ldict_entry_cursor_reduce,
                "native reducer passed an empty page"))
        for index in 1:Int(batch.entry_count)
            entry = copy_entry(batch, unsafe_load(batch.entries, index),
                state.stream.domain)
            if state.stream.prefix === nothing ||
                prefix_matches(first(entry), state.stream.prefix)
                state.accumulator = state.f(state.accumulator, entry)
            elseif !lex_less(first(entry), state.stream.prefix)
                return UInt32(STATUS_END)
            end
        end
        UInt32(STATUS_OK)
    catch error
        state.failure = error
        state.failed = true
        UInt32(STATUS_INVALID_ARGUMENT)
    end
end

"""Fold bounded native pages and close the stream on success or failure.

The reducer callback runs synchronously while each page is leased. Its Julia
exception is caught before returning to native code and rethrown to the caller
after the native lease has been settled.
"""
function fold_entries(f, initial, stream::AlgebraEntries)
    stream.exhausted && return initial
    stream.closed && throw(NativeError(STATUS_CLOSED, :entry_cursor,
        "entry cursor is closed"))
    state = EntryReductionState(f, initial, stream, false, nothing)
    count = Ref{Csize_t}(0)
    callback = @cfunction(reduce_entries_callback, UInt32,
        (Ptr{Cvoid}, Ptr{VTI.VtDictionaryEntryBatchView}))
    try
        status = GC.@preserve state begin
            abi_ldict_entry_cursor_reduce(stream.handle, Ref(stream.limits),
                callback, pointer_from_objref(state), count)
        end
        state.failed && throw(state.failure)
        checked(status, :ldict_entry_cursor_reduce)
        stream.exhausted = true
        state.accumulator
    finally
        close!(stream)
    end
end

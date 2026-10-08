"""Look up keys with one native call, preserving order and duplicate positions.

Each result is `(found, value)`. A present key without a value is
`(true, nothing)`; an absent key is `(false, nothing)`.
"""
function lookup_batch(dictionary::Dictionary, keys)
    require_revision9(dictionary.domain == UNIT_U64 ?
        :ldict_dictionary_get_u64_batch : :ldict_dictionary_get_text_batch)
    key_list = collect(keys)
    found = Vector{UInt8}(undef, length(key_list))
    values = Vector{OptionalU64}(undef, length(key_list))
    if dictionary.domain == UNIT_U64
        buffers = [u64_buffer(key, dictionary.domain) for key in key_list]
        descriptors = [U64Key(isempty(buffer) ? C_NULL : pointer(buffer),
            length(buffer)) for buffer in buffers]
        GC.@preserve buffers descriptors found values begin
            checked(abi_ldict_dictionary_get_u64_batch(require_open(dictionary),
                isempty(descriptors) ? C_NULL : pointer(descriptors),
                length(descriptors), isempty(found) ? C_NULL : pointer(found),
                isempty(values) ? C_NULL : pointer(values)),
                :ldict_dictionary_get_u64_batch)
        end
    else
        buffers = [text_buffer(key, dictionary.domain) for key in key_list]
        descriptors = [TextKey(isempty(buffer) ? C_NULL : pointer(buffer),
            length(buffer)) for buffer in buffers]
        GC.@preserve buffers descriptors found values begin
            checked(abi_ldict_dictionary_get_text_batch(require_open(dictionary),
                isempty(descriptors) ? C_NULL : pointer(descriptors),
                length(descriptors), isempty(found) ? C_NULL : pointer(found),
                isempty(values) ? C_NULL : pointer(values)),
                :ldict_dictionary_get_text_batch)
        end
    end
    results = Vector{Tuple{Bool,Union{Nothing,UInt64}}}(undef, length(key_list))
    for index in eachindex(results)
        found[index] <= 1 && values[index].has_value <= 1 &&
            all(iszero, values[index].reserved) ||
            throw(NativeError(STATUS_PROVIDER_ERROR, :lookup_batch,
                "invalid native batch lookup result"))
        results[index] = (found[index] == 1,
            values[index].has_value == 1 ? values[index].value : nothing)
    end
    results
end

"""Remove keys with one native call and return one Boolean per input key.

Keys are processed in caller order; duplicate keys report `true` only for the
first successful removal. Input is fully validated before mutation starts.
"""
function remove_batch!(dictionary::Dictionary, keys)
    require_revision9(dictionary.domain == UNIT_U64 ?
        :ldict_dictionary_remove_u64_batch : :ldict_dictionary_remove_text_batch)
    key_list = collect(keys)
    removed = Vector{UInt8}(undef, length(key_list))
    if dictionary.domain == UNIT_U64
        buffers = [u64_buffer(key, dictionary.domain) for key in key_list]
        descriptors = [U64Key(isempty(buffer) ? C_NULL : pointer(buffer),
            length(buffer)) for buffer in buffers]
        GC.@preserve buffers descriptors removed begin
            checked(abi_ldict_dictionary_remove_u64_batch(require_open(dictionary),
                isempty(descriptors) ? C_NULL : pointer(descriptors),
                length(descriptors), isempty(removed) ? C_NULL : pointer(removed)),
                :ldict_dictionary_remove_u64_batch)
        end
    else
        buffers = [text_buffer(key, dictionary.domain) for key in key_list]
        descriptors = [TextKey(isempty(buffer) ? C_NULL : pointer(buffer),
            length(buffer)) for buffer in buffers]
        GC.@preserve buffers descriptors removed begin
            checked(abi_ldict_dictionary_remove_text_batch(require_open(dictionary),
                isempty(descriptors) ? C_NULL : pointer(descriptors),
                length(descriptors), isempty(removed) ? C_NULL : pointer(removed)),
                :ldict_dictionary_remove_text_batch)
        end
    end
    all(flag -> flag <= 1, removed) ||
        throw(NativeError(STATUS_PROVIDER_ERROR, :remove_batch!,
            "invalid native batch removal result"))
    Bool[flag == 1 for flag in removed]
end

using Test
using Libdictenstein

const LD = Libdictenstein

@testset "ABI identity and layouts" begin
    @test LD.abi_version() == LD.ABI_VERSION == 1
    @test LD.API_REVISION == 9
    @test LD.api_revision() >= LD.API_REVISION
    @test fieldnames(LD.OptionalU64) == (:value, :has_value, :reserved)
    @test fieldnames(LD.TextEntry) == (:data, :len, :value)
    @test fieldnames(LD.U64Entry) == (:data, :len, :value)
    @test sizeof(LD.OptionalU64) == 16
    @test sizeof(LD.TextEntry) == 32
    @test sizeof(LD.U64Entry) == 32
    @test sizeof(LD.SuffixSourceRecord) == 40
    @test fieldnames(LD.SuffixSourceRecord) == (:source_id, :data, :len, :value)
    @test LD.LdictEntry === LD.VTI.VtDictionaryEntryRaw
    @test LD.LdictEntryBatchLimits === LD.VTI.BatchLimits
    @test LD.LdictEntryBatch === LD.VTI.VtDictionaryEntryBatchView
    @test LD.LdictEntriesInfo === LD.VTI.VtDictionaryEntriesInfo
    @test UInt32(LD.KIND_PERSISTENT_VOCAB_ARTRIE) == 5
    @test LD.KIND_PERSISTENT_VOCABULARY == LD.KIND_PERSISTENT_VOCAB_ARTRIE

    inventory_path = normpath(joinpath(@__DIR__, "..", "..", "..", "generated",
        "julia-abi-capabilities.tsv"))
    inventory = readlines(inventory_path)
    @test split(first(inventory), '\t') == [
        "symbol", "group", "feature", "return_type", "parameters", "julia_wrapper",
        "julia_return_type", "julia_parameter_types", "abi_version", "api_revision",
    ]
    rows = [split(line, '\t'; keepempty=true) for line in inventory[2:end]]
    @test !isempty(rows)
    @test all(length(row) == 10 for row in rows)
    @test length(rows) == length(Set(row[1] for row in rows))
    @test Set(row[1] for row in rows) == Set(
        string(name)[5:end] for name in names(LD; all=true)
        if startswith(string(name), "abi_ldict_")
    )
    @test all(isdefined(LD, Symbol(row[6])) for row in rows)
    @test all(row[9] == string(LD.ABI_VERSION) &&
              row[10] == string(LD.API_REVISION) for row in rows)
end

@testset "revision-9 bounded lazy dictionary algebra" begin
    fixtures = (
        (LD.UNIT_UNICODE_SCALAR, "", "a", "ab", "ac", "b", "a"),
        (LD.UNIT_BYTE, UInt8[], UInt8[0x00], UInt8[0x00, 0xff],
            UInt8[0x01], UInt8[0xff], UInt8[0x00]),
        (LD.UNIT_U64, UInt64[], UInt64[0], UInt64[0, 2],
            UInt64[1], UInt64[2], UInt64[0]),
    )
    for (domain, empty_key, a, ab, ac, b, prefix) in fixtures
        left = LD.DynamicDawg(domain)
        right = LD.DynamicDawg(domain)
        try
            LD.insert_batch!(left, [empty_key => nothing, a => 0, ab => 4])
            LD.insert_batch!(right, [a => 7, ac => nothing, b => 9])

            joined = LD.algebra_entries(left, right;
                operation=LD.ALGEBRA_UNION,
                value_merge=LD.VALUE_MERGE_LATTICE_JOIN,
                page_size=2, max_units=64)
            left[a] = 100
            delete!(right, ac)
            close(left)
            close(right)
            try
                first_page = LD.next_page!(joined)
                @test length(first_page) <= 2
                @test first_page == [empty_key => nothing, a => UInt64(7)]
                rest = collect(joined)
                @test rest == [ab => UInt64(4), ac => nothing, b => UInt64(9)]
                @test !isopen(joined)
                @test LD.next_page!(joined) === nothing
            finally
                close(joined)
            end
        finally
            close(left)
            close(right)
        end

        left = LD.DynamicDawg(domain)
        right = LD.DynamicDawg(domain)
        try
            LD.insert_batch!(left, [empty_key => nothing, a => 0, ab => 4])
            LD.insert_batch!(right, [a => 7, ac => nothing, b => 9])
            expected = (
                (LD.ALGEBRA_UNION, [empty_key => nothing, a => UInt64(7),
                    ab => UInt64(4), ac => nothing, b => UInt64(9)]),
                (LD.ALGEBRA_INTERSECTION, [a => UInt64(7)]),
                (LD.ALGEBRA_DIFFERENCE, [empty_key => nothing, ab => UInt64(4)]),
                (LD.ALGEBRA_SYMMETRIC_DIFFERENCE,
                    [empty_key => nothing, ab => UInt64(4), ac => nothing, b => UInt64(9)]),
            )
            for (operation, entries) in expected
                stream = LD.algebra_entries(left, right;
                    operation, value_merge=LD.VALUE_MERGE_LAST,
                    page_size=1, max_units=64)
                @test collect(stream) == entries
                @test !isopen(stream)
            end
            prefixed = LD.prefix_entries(left, prefix;
                page_size=1, max_units=64)
            @test collect(prefixed) == [a => UInt64(0), ab => UInt64(4)]
            @test !isopen(prefixed)
            joined_prefix = LD.algebra_entries(left, right;
                prefix, page_size=1, max_units=64)
            expected_prefix = domain == LD.UNIT_UNICODE_SCALAR ?
                [a => UInt64(7), ab => UInt64(4), ac => nothing] :
                [a => UInt64(7), ab => UInt64(4)]
            @test collect(joined_prefix) == expected_prefix
            reduced_prefix = LD.algebra_entries(left, right;
                prefix, page_size=1, max_units=64)
            @test LD.fold_entries((count, _) -> count + 1, 0,
                reduced_prefix) == length(expected_prefix)
            @test !isopen(reduced_prefix)
            folded = LD.algebra_entries(left, right;
                page_size=1, max_units=64)
            @test LD.fold_entries((count, _) -> count + 1, 0, folded) == 5
            @test !isopen(folded)
            failed = LD.algebra_entries(left, right)
            @test_throws ErrorException LD.fold_entries((_, _) -> error("stop"), 0,
                failed)
            @test !isopen(failed)
            early = LD.algebra_entries(left, right)
            @test first(early) == (empty_key => nothing)
            close(early)
            @test_throws LD.NativeError LD.next_page!(early)
        finally
            close(left)
            close(right)
        end
    end
end

@testset "revision-9 one-crossing batch lookup and removal" begin
    for (domain, empty_key, key, missing) in (
        (LD.UNIT_UNICODE_SCALAR, "", "é", "absent"),
        (LD.UNIT_BYTE, UInt8[], UInt8[0x00, 0xff], UInt8[0x7f]),
        (LD.UNIT_U64, UInt64[], UInt64[0, typemax(UInt64)], UInt64[1]),
    )
        dictionary = LD.DynamicDawg(domain)
        try
            LD.insert_batch!(dictionary, [empty_key => nothing, key => 0])
            @test LD.lookup_batch(dictionary, [key, missing, empty_key, key]) ==
                [(true, UInt64(0)), (false, nothing), (true, nothing),
                    (true, UInt64(0))]
            @test LD.lookup_batch(dictionary, ()) == Tuple{Bool,Union{Nothing,UInt64}}[]
            @test LD.remove_batch!(dictionary, [key, missing, empty_key, key]) ==
                [true, false, true, false]
            @test LD.remove_batch!(dictionary, ()) == Bool[]
            @test isempty(dictionary)
        finally
            close(dictionary)
        end
        @test_throws LD.NativeError LD.lookup_batch(dictionary, [key])
    end
end

@testset "revision-8 PathMap dictionary" begin
    raw = LD.PathMap(LD.UNIT_BYTE)
    unicode = LD.PathMap()
    try
        @test LD.kind(raw) == LD.KIND_PATHMAP
        @test LD.capabilities(raw) ==
            LD.CAP_READ | LD.CAP_INSERT | LD.CAP_REMOVE | LD.CAP_CLEAR
        raw[UInt8[0x00, 0xff, 0x80]] = UInt64(0)
        @test raw[UInt8[0x00, 0xff, 0x80]] == 0
        @test LD.insert_batch!(raw, [UInt8[0xff] => nothing,
            UInt8[0x80, 0x00] => 9]) == 2
        @test Set(keys(raw)) == Set([UInt8[0x00, 0xff, 0x80],
            UInt8[0xff], UInt8[0x80, 0x00]])
        unicode["é🙂"] = 7
        @test unicode["é🙂"] == 7
        captured = LD.snapshot(raw)
        empty!(raw)
        try
            @test length(captured) == 3
        finally
            close(captured)
        end
    finally
        close(raw)
        close(unicode)
    end
    @test_throws LD.NativeError LD.PathMap(LD.UNIT_U64)
end

@testset "revision-8 typed suffix-source snapshots" begin
    index = LD.SuffixIndex(LD.UNIT_BYTE)
    try
        for (text, value) in [("aba", UInt64(0)), ("aba", nothing),
            ("ababa", UInt64(7)), ("", nothing)]
            LD.insert_source!(index, text, value)
        end
        view = LD.source_snapshot(index)
        try
            @test length(view) == 4
            @test LD.contains_source(view, "aba")
            @test !LD.contains_source(view, "ba")
            @test LD.contains_substring(view, "ba")
            @test LD.substring_frequency(view, "aba") == 4
            @test LD.substring_frequency(view, "") == 15
            records = LD.source_records(view; page_size=2)
            @test getfield.(records, :source_id) == [3, 0, 1, 2]
            @test getfield.(records, :text) == ["", "aba", "aba", "ababa"]
            @test getfield.(records, :value) == [nothing, UInt64(0), nothing, UInt64(7)]
            identity = LD.source_identity(view)
            @test identity.producer != 0
            @test identity.revision == 4
            @test LD.remove_source!(index, "aba")
            later = LD.source_snapshot(index)
            try
                @test LD.substring_frequency(later, "aba") == 3
            finally
                close(later)
            end
            @test LD.substring_frequency(view, "aba") == 4
            @test LD.compact!(index) === index
            empty!(index)
            LD.insert_source!(index, "new")
            after_clear = LD.source_snapshot(index)
            try
                @test first(LD.source_records(after_clear)).source_id == 0
                @test LD.source_identity(after_clear).revision > identity.revision
            finally
                close(after_clear)
            end
            close(index)
            @test LD.substring_frequency(view, "aba") == 4
        finally
            close(view)
        end
    finally
        close(index)
    end
    scalar = LD.SuffixIndex(LD.UNIT_UNICODE_SCALAR)
    bytes = LD.SuffixIndex(LD.UNIT_BYTE)
    try
        for index in (scalar, bytes)
            LD.insert_source!(index, "é🙂é")
            LD.insert_source!(index, "")
        end
        scalar_view = LD.source_snapshot(scalar)
        byte_view = LD.source_snapshot(bytes)
        try
            @test LD.substring_frequency(scalar_view, "") == 5
            @test LD.substring_frequency(byte_view, "") == 10
            @test LD.substring_frequency(scalar_view, "é") == 2
        finally
            close(scalar_view)
            close(byte_view)
        end
    finally
        close(scalar)
        close(bytes)
    end
    @test_throws LD.NativeError LD.SuffixIndex(LD.UNIT_U64)
end

@testset "Unicode AbstractDict and snapshot iteration" begin
    dictionary = LD.DynamicDawg()
    try
        @test isempty(dictionary)
        dictionary["cat"] = UInt64(7)
        dictionary["cot"] = nothing
        dictionary["zero"] = UInt64(0)
        @test length(dictionary) == 3
        @test dictionary["cat"] == 7
        @test dictionary["cot"] === nothing
        @test dictionary["zero"] == 0
        @test get(dictionary, "missing", :absent) === :absent
        @test_throws KeyError dictionary["missing"]
        @test Set(keys(dictionary)) == Set(["cat", "cot", "zero"])
        @test keys(dictionary) isa AbstractSet{String}
        @test union(keys(dictionary), Set(["new"])) ==
            Set(["cat", "cot", "zero", "new"])

        view = LD.snapshot(dictionary)
        delete!(dictionary, "cat")
        try
            @test Dict(view)["cat"] == 7
            @test !haskey(dictionary, "cat")
        finally
            close(view)
        end

        @test LD.insert_batch!(dictionary,
            ["ant" => 1, "bee" => nothing, "eel" => 3]) == 3
        @test dictionary["bee"] === nothing
        @test LD.kind(dictionary) == LD.KIND_DYNAMIC_DAWG
        @test LD.capabilities(dictionary) & LD.CAP_INSERT != 0
        @test LD.compact!(dictionary) >= 0
    finally
        close(dictionary)
    end
    @test !isopen(dictionary)
    @test_throws LD.NativeError length(dictionary)
end

@testset "byte and u64 key domains" begin
    bytes = LD.DynamicDawg(LD.UNIT_BYTE)
    tokens = LD.DynamicDawg(LD.UNIT_U64)
    try
        raw = UInt8[0x00, 0xff, 0x41]
        bytes[raw] = 5
        @test bytes[raw] == 5
        tokens[UInt64[1, 2, typemax(UInt64)]] = nothing
        @test haskey(tokens, UInt64[1, 2, typemax(UInt64)])
        @test tokens[UInt64[1, 2, typemax(UInt64)]] === nothing
    finally
        close(bytes)
        close(tokens)
    end
end

@testset "sorted minimal DynamicDAWG builder" begin
    unicode = LD.SortedMinimalDawg([
        "" => nothing, "café" => 1, "café" => 2, "中文" => 0])
    bytes = LD.SortedMinimalDawg([
        UInt8[0x00] => nothing, UInt8[0x00, 0xff] => 4,
        UInt8[0xff] => 7]; domain=LD.UNIT_BYTE)
    tokens = LD.SortedMinimalDawg([
        UInt64[] => nothing, UInt64[1] => 5, UInt64[1, 2] => 6];
        domain=LD.UNIT_U64)
    empty = LD.SortedMinimalDawg((); domain=LD.UNIT_BYTE)
    try
        @test LD.kind(unicode) == LD.KIND_DYNAMIC_DAWG
        @test length(unicode) == 3
        @test unicode[""] === nothing
        @test unicode["café"] == 2
        @test unicode["中文"] == 0
        unicode["later"] = 8
        @test unicode["later"] == 8

        @test bytes[UInt8[0x00]] === nothing
        @test bytes[UInt8[0x00, 0xff]] == 4
        @test bytes[UInt8[0xff]] == 7
        @test tokens[UInt64[]] === nothing
        @test tokens[UInt64[1, 2]] == 6
        @test isempty(empty)
    finally
        foreach(close, (unicode, bytes, tokens, empty))
    end
    @test_throws ArgumentError LD.SortedMinimalDawg(["z" => 1, "a" => 2])
    @test_throws ArgumentError LD.SortedMinimalDawg([
        UInt8[0xff] => 1, UInt8[0x00] => 2]; domain=LD.UNIT_BYTE)
    @test_throws ArgumentError LD.SortedMinimalDawg([
        UInt64[2] => 1, UInt64[1] => 2]; domain=LD.UNIT_U64)
    @test_throws ArgumentError LD.SortedMinimalDawg(["a" => -1])
    @test_throws ArgumentError LD.SortedMinimalDawg(["a" => 1]; domain=LD.UNIT_U64)
end

@testset "backend-specific operations" begin
    dat = LD.DoubleArrayTrie(["cat" => 1, "dog" => nothing])
    suffix = LD.Scdawg()
    try
        @test dat["cat"] == 1
        @test_throws LD.NativeError (dat["new"] = 2)
        suffix["banana"] = 3
        suffix["bandana"] = nothing
        @test LD.contains_substring(suffix, "ana")
        @test LD.substring_frequency(suffix, "ana") >= 2
    finally
        close(dat)
        close(suffix)
    end
end

@testset "native dictionary algebra and value lattices" begin
    left = LD.DynamicDawg()
    right = LD.DynamicDawg()
    try
        LD.insert_batch!(left, ["left" => 1, "shared" => 4, "valueless" => nothing])
        LD.insert_batch!(right, ["right" => 2, "shared" => 9, "valueless" => 7])

        joined = LD.algebra(left, right, LD.ALGEBRA_UNION,
            LD.VALUE_MERGE_LATTICE_JOIN)
        met = LD.intersection(left, right)
        only_left = LD.difference(left, right)
        exclusive = LD.symmetric_difference(left, right)
        natural = merge(left, right)
        try
            @test Dict(joined) == Dict(
                "left" => 1, "right" => 2, "shared" => 9, "valueless" => 7)
            @test Dict(met) == Dict("shared" => 4, "valueless" => nothing)
            @test Dict(only_left) == Dict("left" => 1)
            @test Set(keys(exclusive)) == Set(["left", "right"])
            @test natural["shared"] == 9

            left["later"] = 99
            @test !haskey(joined, "later")
            joined["result-only"] = 10
            @test !haskey(left, "result-only")
        finally
            foreach(close, (joined, met, only_left, exclusive, natural))
        end
    finally
        close(left)
        close(right)
    end
end

@testset "persistent lifecycle" begin
    parent = get(ENV, "LIBDICTENSTEIN_TEST_SCRATCH", joinpath(@__DIR__, "target"))
    mkpath(parent)
    directory = mktempdir(parent)
    path = joinpath(directory, "dictionary")
    created = LD.PersistentARTrie(path; create=true)
    try
        created["durable"] = 41
        LD.checkpoint!(created)
    finally
        close(created)
    end
    reopened = LD.PersistentARTrie(path; create=false)
    try
        @test reopened["durable"] == 41
    finally
        close(reopened)
    end
end

@testset "type stability and bounded lookup allocation" begin
    dictionary = LD.DynamicDawg()
    try
        dictionary["stable"] = 1
        @test @inferred(length(dictionary)) == 1
        @test @inferred(haskey(dictionary, "stable"))
        haskey(dictionary, "stable")
        @test @allocated(haskey(dictionary, "stable")) <= 512
    finally
        close(dictionary)
    end
end

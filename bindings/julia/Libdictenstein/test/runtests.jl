using Test
using Libdictenstein

const LD = Libdictenstein

@testset "ABI identity and layouts" begin
    @test LD.abi_version() == LD.ABI_VERSION == 1
    @test LD.api_revision() == LD.API_REVISION == 6
    @test fieldnames(LD.OptionalU64) == (:value, :has_value, :reserved)
    @test fieldnames(LD.TextEntry) == (:data, :len, :value)
    @test fieldnames(LD.U64Entry) == (:data, :len, :value)
    @test sizeof(LD.OptionalU64) == 16
    @test sizeof(LD.TextEntry) == 32
    @test sizeof(LD.U64Entry) == 32
    @test LD.LdictEntry === LD.VTI.VtDictionaryEntryRaw
    @test LD.LdictEntryBatchLimits === LD.VTI.BatchLimits
    @test LD.LdictEntryBatch === LD.VTI.VtDictionaryEntryBatchView
    @test LD.LdictEntriesInfo === LD.VTI.VtDictionaryEntriesInfo
    @test UInt32(LD.KIND_PERSISTENT_VOCAB_ARTRIE) == 5
    @test LD.KIND_PERSISTENT_VOCABULARY == LD.KIND_PERSISTENT_VOCAB_ARTRIE

    inventory_path = normpath(joinpath(@__DIR__, "..", "..", "..", "generated",
        "julia-abi-capabilities.tsv"))
    inventory = readlines(inventory_path)
    @test length(inventory) == 43
    @test split(first(inventory), '\t') == [
        "symbol", "group", "feature", "return_type", "parameters", "julia_wrapper",
        "julia_return_type", "julia_parameter_types", "abi_version", "api_revision",
    ]
    rows = [split(line, '\t'; keepempty=true) for line in inventory[2:end]]
    @test all(length(row) == 10 for row in rows)
    @test Set(row[1] for row in rows) == Set(
        string(name)[5:end] for name in names(LD; all=true)
        if startswith(string(name), "abi_ldict_")
    )
    @test all(isdefined(LD, Symbol(row[6])) for row in rows)
    @test all(row[9] == "1" && row[10] == "6" for row in rows)
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

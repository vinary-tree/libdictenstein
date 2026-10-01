using Libdictenstein
using Test

@testset "revision-7 library rejects new symbols before lookup" begin
    @test abi_version() == 1
    @test api_revision() == 7
    for (feature, constructor) in ((:ldict_pathmap_new, () -> PathMap(UNIT_BYTE)),
                                    (:ldict_suffix_index_new, () -> SuffixIndex(UNIT_BYTE)))
        error = try
            constructor()
            nothing
        catch caught
            caught
        end
        @test error isa NativeError
        @test Int(error.status) == 6
        @test error.operation == feature
    end
end

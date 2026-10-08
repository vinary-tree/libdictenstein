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
    closed = Dictionary{String}(C_NULL, UNIT_UNICODE_SCALAR, true)
    for (feature, call) in (
        (:ldict_dictionary_algebra_entries_open,
            () -> algebra_entries(closed, closed)),
        (:ldict_dictionary_get_text_batch, () -> lookup_batch(closed, ())),
        (:ldict_dictionary_remove_text_batch, () -> remove_batch!(closed, ())),
    )
        error = try
            call()
            nothing
        catch caught
            caught
        end
        @test error isa NativeError
        @test error.status == Libdictenstein.STATUS_UNSUPPORTED
        @test error.operation == feature
    end
end

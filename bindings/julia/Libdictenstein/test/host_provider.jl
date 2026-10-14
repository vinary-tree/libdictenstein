import VinaryTreeInterop as VTI
import Libdictenstein: provider_capture, provider_unit_domain,
    provider_value_domain, provider_root, provider_isfinal, provider_value,
    provider_length, provider_edge_page, provider_identity, provider_graph,
    provider_graph_value, provider_entries

struct HostFixture <: LD.AbstractHostDictionarySnapshot
    domain::VTI.UnitDomain
    label::UInt64
    revision::UInt64
    bad_edges::Bool
    bad_entries::Bool
    compact::Bool
end

struct HostSource
    current::Base.RefValue{HostFixture}
end
provider_capture(source::HostSource) = source.current[]
provider_unit_domain(fixture::HostFixture) = fixture.domain
provider_value_domain(::HostFixture) = VTI.VALUE_OPTIONAL_U64
provider_root(::HostFixture) = UInt64(0)
provider_length(::HostFixture) = 2
provider_isfinal(::HostFixture, node::UInt64) = node in (0, 1)
provider_value(fixture::HostFixture, node::UInt64) =
    node == 0 ? fixture.revision : nothing
provider_graph_value(fixture::HostFixture, cursor::UInt64) =
    provider_value(fixture, cursor - 1)
provider_identity(fixture::HostFixture) =
    VTI.SnapshotIdentity(47, fixture.revision)

function provider_edge_page(fixture::HostFixture, node::UInt64,
    start::Int, capacity::Int)
    fixture.bad_edges && throw(ArgumentError("hostile edge source"))
    node in (0, 1) || throw(ArgumentError("node is outside the snapshot"))
    edges = node == 0 ? VTI.VtDictionaryEdge[
        VTI.VtDictionaryEdge(fixture.label, 1)] : VTI.VtDictionaryEdge[]
    start <= length(edges) || throw(ArgumentError("edge offset is invalid"))
    (edges[start + 1:min(length(edges), start + capacity)], length(edges))
end

function provider_entries(fixture::HostFixture)
    unit = fixture.domain == VTI.UNIT_BYTE ? UInt8(fixture.label) :
        fixture.domain == VTI.UNIT_UNICODE_SCALAR ? UInt32(fixture.label) :
        fixture.label
    key = [unit]
    fixture.bad_entries ? [key => nothing, key => nothing] :
        [typeof(key)() => fixture.revision, key => nothing]
end

function provider_graph(fixture::HostFixture)
    fixture.compact || return nothing
    zero = ntuple(_ -> UInt8(0), 7)
    LD.HostDictionaryGraph(VTI.VtDictionaryGraphNode[
        VTI.VtDictionaryGraphNode(0, 1, 1, 1, zero),
        VTI.VtDictionaryGraphNode(1, 0, 2, 1, zero)],
        VTI.VtDictionaryGraphEdge[
            VTI.VtDictionaryGraphEdge(fixture.label, 1)], 0)
end

function host_key(fixture::HostFixture)
    fixture.domain == VTI.UNIT_BYTE && return UInt8[UInt8(fixture.label)]
    fixture.domain == VTI.UNIT_UNICODE_SCALAR &&
        return string(Char(UInt32(fixture.label)))
    UInt64[fixture.label]
end

struct HostSetSnapshot <: LD.AbstractHostDictionarySnapshot end
provider_unit_domain(::HostSetSnapshot) = VTI.UNIT_BYTE
provider_value_domain(::HostSetSnapshot) = VTI.VALUE_UNIT
provider_root(::HostSetSnapshot) = UInt64(0)
provider_isfinal(::HostSetSnapshot, node::UInt64) = node == 1
function provider_edge_page(::HostSetSnapshot, node::UInt64,
    start::Int, capacity::Int)
    edges = node == 0 ? [VTI.VtDictionaryEdge(UInt64('a'), 1)] :
        VTI.VtDictionaryEdge[]
    start <= length(edges) || throw(ArgumentError("invalid offset"))
    (edges[start + 1:min(length(edges), start + capacity)], length(edges))
end

@testset "Julia-hosted unit-valued set and optional capabilities" begin
    dictionary = LD.host_dictionary(HostSetSnapshot())
    try
        @test VTI.known_length(dictionary) === nothing
        @test VTI.snapshot_identity(dictionary) === nothing
        @test VTI.graph(dictionary) === nothing
        @test_throws VTI.InteropError VTI.entries(dictionary)
        @test haskey(dictionary, UInt8['a'])
        @test dictionary[UInt8['a']] === nothing
        @test !haskey(dictionary, UInt8['b'])
    finally
        close(dictionary)
    end
end

@testset "Julia-hosted dictionary resources and lifecycle" begin
    for (domain, label) in ((VTI.UNIT_BYTE, UInt64(0)),
        (VTI.UNIT_UNICODE_SCALAR, UInt64('é')),
        (VTI.UNIT_U64, typemax(UInt64)))
        original = HostFixture(domain, label, 0, false, false, true)
        source = HostSource(Ref(original))
        dictionary = LD.host_dictionary(source)
        @test VTI.flags(dictionary) & VTI.DICTIONARY_FLAG_IMMUTABLE != 0
        @test VTI.flags(dictionary) & VTI.DICTIONARY_FLAG_PARALLEL_REENTRANT == 0
        @test VTI.unit_domain(dictionary) == domain
        @test VTI.known_length(dictionary) == 2
        @test dictionary[domain == VTI.UNIT_BYTE ? UInt8[] :
            domain == VTI.UNIT_UNICODE_SCALAR ? "" : UInt64[]] == UInt64(0)
        @test dictionary[host_key(original)] === nothing
        @test VTI.visit(dictionary, 0; batch_size=1)[1]
        @test only(VTI.edges(dictionary, 0; batch_size=1)).label == label
        @test VTI.snapshot_identity(dictionary) == VTI.SnapshotIdentity(47, 0)

        graph = VTI.graph(dictionary)
        @test length(VTI.graph_nodes(graph)) == 2
        @test VTI.value(graph, 1) == UInt64(0)
        captured = VTI.snapshot(dictionary)
        source.current[] = HostFixture(domain, label, 19, false, false, true)
        newer = LD.host_dictionary(source)
        @test newer[domain == VTI.UNIT_BYTE ? UInt8[] :
            domain == VTI.UNIT_UNICODE_SCALAR ? "" : UInt64[]] == UInt64(19)
        close(newer)
        close(dictionary)
        @test VTI.value(graph, 1) == UInt64(0)
        @test captured[domain == VTI.UNIT_BYTE ? UInt8[] :
            domain == VTI.UNIT_UNICODE_SCALAR ? "" : UInt64[]] == UInt64(0)
        close(graph)
        close(captured)
    end
end

@testset "Julia-hosted entry pages, limits, cancellation, and reduction" begin
    dictionary = LD.host_dictionary(HostFixture(VTI.UNIT_BYTE, UInt64('a'),
        7, false, false, true))
    try
        cursor = VTI.entries(dictionary)
        @test VTI.known_length(cursor) == 2
        @test VTI.snapshot_identity(cursor) == VTI.SnapshotIdentity(47, 7)
        first_page = VTI.next_batch(cursor, VTI.BatchLimits(1, 0, 1))
        @test only(VTI.copied_entries(first_page)).value == UInt64(7)
        @test_throws VTI.InteropError VTI.next_batch(cursor)
        VTI.release!(first_page)
        @test_throws VTI.InteropError VTI.next_batch(cursor,
            VTI.BatchLimits(1, 0, 1))
        second_page = VTI.next_batch(cursor, VTI.BatchLimits(1, 1, 0))
        @test only(VTI.copied_entries(second_page)).units == UInt8['a']
        @test only(VTI.copied_entries(second_page)).value === nothing
        VTI.release!(second_page)
        @test VTI.next_batch(cursor) === nothing
        close(cursor)

        cursor = VTI.entries(dictionary)
        seen = Union{Nothing,UInt64}[]
        @test VTI.reduce_entries(page -> (push!(seen, only(page).value);
            nothing), cursor, VTI.BatchLimits(1, 1, 1)) == 2
        @test seen == Union{Nothing,UInt64}[7, nothing]
        close(cursor)

        cursor = VTI.entries(dictionary)
        VTI.cancel!(cursor)
        @test VTI.next_batch(cursor) === nothing
        close(cursor)
    finally
        close(dictionary)
    end
end

@testset "Julia-hosted malformed callback containment and budgets" begin
    bad_edges = LD.host_dictionary(HostFixture(VTI.UNIT_BYTE,
        UInt64('a'), 1, true, false, false))
    try
        @test VTI.graph(bad_edges) === nothing
        @test_throws VTI.InteropError VTI.edges(bad_edges, 0)
    finally
        close(bad_edges)
    end
    bad_entries = LD.host_dictionary(HostFixture(VTI.UNIT_BYTE,
        UInt64('a'), 1, false, true, false))
    cursor = VTI.entries(bad_entries)
    try
        @test_throws VTI.InteropError VTI.next_batch(cursor,
            VTI.BatchLimits(2, 4, 2))
        @test VTI.next_batch(cursor) === nothing
    finally
        close(cursor)
        close(bad_entries)
    end
    @test_throws ArgumentError LD.host_dictionary(HostFixture(
        VTI.UNIT_BYTE, UInt64('a'), 1, false, false, true);
        max_graph_nodes=1)
    @test_throws ArgumentError LD.host_dictionary(HostFixture(
        VTI.UNIT_BYTE, UInt64('a'), 1, false, false, true);
        max_graph_edges=0)
    zero = ntuple(_ -> UInt8(0), 7)
    @test_throws ArgumentError LD.copy_host_graph(LD.HostDictionaryGraph(
        [VTI.VtDictionaryGraphNode(0, 0, 0, 1, zero)],
        VTI.VtDictionaryGraphEdge[], 0), UInt32(VTI.UNIT_BYTE), 1, 0)
end

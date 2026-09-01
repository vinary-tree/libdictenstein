using Documenter
using Libdictenstein

DocMeta.setdocmeta!(Libdictenstein, :DocTestSetup,
    :(using Libdictenstein); recursive=true)

makedocs(
    modules=[Libdictenstein],
    sitename="Libdictenstein.jl",
    strict=true,
    doctest=true,
    pages=[
        "Guide" => "index.md",
        "API" => "api.md",
    ],
)

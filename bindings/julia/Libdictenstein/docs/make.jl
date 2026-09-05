using Documenter
using Libdictenstein

DocMeta.setdocmeta!(Libdictenstein, :DocTestSetup,
    :(using Libdictenstein); recursive=true)

makedocs(
    modules=[Libdictenstein],
    sitename="Libdictenstein.jl",
    checkdocs=:exports,
    doctest=true,
    warnonly=false,
    pages=[
        "Guide" => "index.md",
        "API" => "api.md",
    ],
)

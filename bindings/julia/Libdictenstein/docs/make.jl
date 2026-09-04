using Documenter
using Libdictenstein

DocMeta.setdocmeta!(Libdictenstein, :DocTestSetup,
    :(using Libdictenstein); recursive=true)

makedocs(
    modules=[Libdictenstein],
    sitename="Libdictenstein.jl",
    # Documenter 1.x removed the legacy `strict` keyword.  An empty
    # `warnonly` list preserves fail-closed documentation diagnostics, while
    # `checkdocs=:all` retains strict API coverage checking.
    warnonly=Symbol[],
    checkdocs=:all,
    doctest=true,
    pages=[
        "Guide" => "index.md",
        "API" => "api.md",
    ],
)

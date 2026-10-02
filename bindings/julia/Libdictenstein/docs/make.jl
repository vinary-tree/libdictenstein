using Documenter
using Libdictenstein

DocMeta.setdocmeta!(Libdictenstein, :DocTestSetup,
    :(using Libdictenstein); recursive=true)

makedocs(
    root=@__DIR__,
    build="build",
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

isfile(joinpath(@__DIR__, "build", "index.html")) ||
    error("Documenter did not generate docs/build/index.html")

# Only the development-docs workflow enables deployment. Julia package
# registration and versioned documentation are separate release gates.
if get(ENV, "LIBDICTENSTEIN_DOCS_DEPLOY", "0") == "1"
    deploydocs(
        root=@__DIR__,
        target="build",
        repo="github.com/vinary-tree/libdictenstein.git",
        devbranch="master",
        push_preview=false,
    )
end

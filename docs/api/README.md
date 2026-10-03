# Managed-language API references

These are RC.6 **source-candidate** references. Generating or checking them
does not imply that RC.6 has been published to a package registry.

| Language | Authoritative API | Browsable reference and checked usage |
|---|---|---|
| Java, Kotlin, Scala | Java sources under [`bindings/jvm/src/main/java`](../../bindings/jvm/src/main/java); Kotlin and Scala consume the same Maven artifact | Strict generated Javadoc; [Kotlin](../../bindings/jvm/kotlin.md) and [Scala](../../bindings/jvm/scala.md) compile-checked guides |
| C#, F# | C# sources under [`bindings/dotnet/src`](../../bindings/dotnet/src); F# consumes the same NuGet assembly | [DocFX source](dotnet/index.md) and [F# guide](../../bindings/dotnet/fsharp.md) |
| Swift | Swift sources under [`bindings/swift/libdictenstein/Sources`](../../bindings/swift/libdictenstein/Sources) | [DocC catalog](../../bindings/swift/libdictenstein/Sources/Libdictenstein/Libdictenstein.docc/Libdictenstein.md) and [Swift guide](../../bindings/swift/README.md) |

The [binding-conformance workflow](../../.github/workflows/bindings-conformance.yml)
builds the Java, .NET, and Swift sites without publishing. It then checks
browsable entry points and compares generated type pages against each
source facade's complete public type inventory. The Kotlin, Scala, and F#
examples are compiled against those exact facades; no unshipped wrapper or
separately published language artifact is claimed.

The `validate-only` release job stages a deterministic DocFX archive and
digest manifest before making the GitHub release immutable. The protected,
manual [DocFX deployment](../../.github/workflows/dotnet-docs-release.yml)
rebuilds and compares that archive, preserves prior `gh-pages` versions, and
checks every served file byte at the versioned Pages URL. It must not be
dispatched before RC.6 release review. Maven Central carries the Java
`-javadoc.jar`, and NuGet carries the XML comments with both target
frameworks, with a [package-specific NuGet README](../../bindings/dotnet/NUGET.md)
that links to the F# guide rather than the Rust crate's README. After those
packages and Swift Package Index DocC are public,
the read-only
[registry gate](../../.github/workflows/managed-api-registry-readback.yml)
verifies their exact-version contents. A source build or local DocC site is
not evidence of registry publication.

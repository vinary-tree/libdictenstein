# Releasing libdictenstein

This guide is the operator contract for libdictenstein's `4.0.0-rc.4` source,
native, and language-binding artifacts. The family-wide dependency order,
registry spellings, and credential matrix remain normative in
[liblevenshtein's release guide](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/releasing-language-bindings.md).

## Identity and prerequisites

`release/version.json` is the version authority. The root crate, package
manifests, native metadata, and language facades must agree with it before a
tag is created. The release tag is `v4.0.0-rc.4`; Go later receives the
additional subdirectory tag `bindings/go/v4.0.0-rc.4`.

The source validation graph consumes exact `v4.0.0-rc.4` tags for
`vinary-tree-interop` and liblevenshtein plus `v0.1.0` for `llattice`.
liblevenshtein is a cross-project validation consumer, not a registry
prerequisite: public libdictenstein packages are published after interop and
before liblevenshtein.

Before tagging, require a clean worktree and run:

```bash
python3 scripts/sync-release-version.py
python3 scripts/check-bindings.py
python3 scripts/check-binding-docs.py
RUST_BACKTRACE=1 cargo nextest run --release --no-fail-fast --workspace --all-features
cargo clippy --all-features --all-targets -- -D warnings
```

The synchronizer writes and validates the libdictenstein and interop package
entries in the primary `Cargo.lock`. Locked builds must leave that file
byte-for-byte unchanged; a stale lock is a source defect, not something a
publication workflow may regenerate.

The repository CI additionally proves the feature matrix, Rocq models,
sanitizers, documentation, diagrams, and all language conformance suites.

## Two-phase workflow

Pushing the tag creates only the immutable source ref. A manual
`validate-only` dispatch at the tag validates and stages artifacts and creates
the checksummed GitHub prerelease. Later publication dispatches must choose one
`registry` value; branch dispatches fail the contract job.

```bash
gh workflow run release-bindings.yml \
  --repo vinary-tree/libdictenstein \
  --ref v4.0.0-rc.4 \
  -f registry=validate-only

gh workflow run release-bindings.yml \
  --repo vinary-tree/libdictenstein \
  --ref v4.0.0-rc.4 \
  -f registry=npm
```

`validate-only` enables no registry uploader. The other choices—`npm`,
`crates-io`, `pypi`, `maven-central`, `clojars`, `nuget`, `rubygems`,
`go-module`, `luarocks`, and `opam`—each authorize only their matching
protected job. There is deliberately no publish-all option.

## Keyless registry authentication

The crates.io job uses OpenID Connect (OIDC) trusted publishing rather than a
stored Cargo token. In the `libdictenstein` crate settings on crates.io,
register repository `vinary-tree/libdictenstein`, workflow
`release-bindings.yml`, and environment `crates-io`. The job grants
`id-token: write` only to the uploader, obtains a temporary token with
`rust-lang/crates-io-auth-action@v1`, and passes that value to
`cargo publish --locked`.

npm uses the corresponding package-level publisher for
`@vinary-tree/libdictenstein`: repository `vinary-tree/libdictenstein`,
workflow `release-bindings.yml`, environment `npm`, and direct `npm publish`
authority. After the first successful keyless publications, require trusted
publishing on crates.io, disallow npm tokens in the package settings, and
revoke the superseded long-lived credentials. The family-wide guide contains
the complete publisher matrix and recovery order.

RubyGems is keyless as well. Because `libdictenstein` is a new global gem
coordinate, register a pending trusted publisher with repository owner
`vinary-tree`, repository `libdictenstein`, workflow `release-bindings.yml`,
and environment `rubygems`; leave reusable-workflow fields empty. The uploader
alone receives `id-token: write`, exchanges that identity through the official
RubyGems credential action pinned to release `v2.1.0`'s immutable commit, and
pushes the exact `.gem` produced by the unprivileged package job. No
`RUBYGEMS_API_KEY` is stored.

Clojars does not offer a GitHub OIDC exchange. Verify `io.vinarytree` in
Clojars using the `vinarytree.io` DNS proof, store the public account name as
the organization variable `CLOJARS_USERNAME`, and store only
`CLOJARS_DEPLOY_TOKEN` in this repository's protected `clojars` environment.
The first `io.vinarytree/libdictenstein-clojure` upload requires an unscoped,
single-use bootstrap token because Clojars cannot scope a token to a nonexistent
artifact. After registry read-back succeeds, disable it and replace it with a
finite-expiration token scoped only to that artifact.

LuaRocks has no OIDC trusted-publisher exchange. Create an API key dedicated to
`vinary-tree/libdictenstein` and store it only as `LUAROCKS_API_KEY` in this
repository's protected `luarocks` environment; do not share the
`liblevenshtein-rust` key or place either key at organization scope. The upload
job uses `--temp-key`, which authenticates this invocation without persisting
the secret in the runner's LuaRocks configuration. Required-reviewer protection
remains the human authorization boundary for each upload.

The source rock deliberately treats the native SDK as an external dependency.
Its module paths use `LIBDICTENSTEIN_INCDIR` and
`LIBDICTENSTEIN_LIBDIR`, followed by the source package's generated shared-ABI
headers; never replace them with a checkout-local `target/release` path. Before
upload, the release job downloads the exact Linux native SDK, builds the rock
into an isolated tree through those two variables, and runs the public Lua
conformance program from that installed tree. `luarocks lint` alone is
insufficient because it does not invoke the linker or load the resulting
module.

opam publication targets the fixed organization fork
`vinary-tree/opam-repository` and opens an upstream pull request against
`ocaml/opam-repository:master`. Store a short-lived classic GitHub token with
only `public_repo` as `OPAM_GITHUB_TOKEN` in this repository's protected
`opam` environment. The job checks out the release model, reads the opam-native
`4.0.0~rc4` version for the package directory, uses the canonical version only
in its Git-safe branch name, and configures Git authentication without placing
the token in a remote URL. Submit `vinary-tree-interop.4.0.0~rc4` first; only
after that upstream package is merged and publicly resolvable should this job
submit `libdictenstein.4.0.0~rc4`. Revoke the release token after the complete
three-package submission sequence.

The `validate-only` graph does not mutate a package registry, but its terminal
job writes the checksummed GitHub prerelease. Protect that job with the
`github-release` environment and a required reviewer. It needs no stored
secret; approval gates the job-scoped `GITHUB_TOKEN` used for the release.

The renamed global-distribution metadata first appeared in append-only source
`v4.0.0-rc.4-release.1`. Its validate-only graph passed every package lane
except the numeric fpm candidate: fpm correctly rejected the intentional public
module `vinary_tree_libdictenstein` while optional package-name enforcement was
enabled for package `libdictenstein`. No registry publication job ran.

Append-only source `v4.0.0-rc.4-release.2` preserves that idiomatic namespaced
Fortran module and disables only fpm's optional module-name convention in both
development and staged manifests. Publisher-environment corrections are
preserved by releases 3 and 4. Public read-back of the revision-1 Lua rock then
proved that its hard-coded source-tree library path was not installable from a
registry archive. Append-only source `v4.0.0-rc.4-release.5` replaces that path
with LuaRocks' external-dependency variables and publishes packaging revision
`4.0.0rc4-2`. The Rust RC, ABI, and Lua API remain unchanged. The synchronizer
and binding contract treat the semantic version, Lua packaging revision, and
source tag as independent release invariants. The opam staging job derives its
own exact corrective source ref from `GITHUB_REF_NAME`.

## Artifact evidence

The GitHub prerelease retains portable native archives, Python wheels, the npm
tarball, Maven and Clojure staging files, NuGet and Ruby packages, the
LuaRocks and opam metadata, the Hackage numeric candidate, and one
`SHA256SUMS` manifest. Hackage and fpm remain candidate-only for the RC
because their numeric `4.0.0` spellings cannot distinguish it from the final
release.

For npm, the protected job publishes
`@vinary-tree/libdictenstein@4.0.0-rc.4` with provenance under `next`.
Install the public tarball in a clean directory, exercise dictionary
construction, mutation, iteration, snapshot stability, and deterministic
close, then move the new scoped package's `latest` tag to the RC, remove
`bootstrap`, and deprecate the immutable `0.0.0` reservation artifact.

## Failure discipline

Tags and published versions are immutable. A failed validation run may be
rerun with `registry=validate-only`; a failed registry lane may be retried only
when the registry confirms that version was not accepted. If public bytes are
wrong, repair the source and issue the next unused candidate. Never move the
source tag, overwrite a package version, or authorize an unrelated registry
to compensate for a failed lane.

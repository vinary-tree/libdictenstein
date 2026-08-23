# Releasing libdictenstein

This guide is the operator contract for libdictenstein's `4.0.0-rc.1` source,
native, and language-binding artifacts. The family-wide dependency order,
registry spellings, and credential matrix remain normative in
[liblevenshtein's release guide](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/releasing-language-bindings.md).

## Identity and prerequisites

`release/version.json` is the version authority. The root crate, package
manifests, native metadata, and language facades must agree with it before a
tag is created. The release tag is `v4.0.0-rc.1`; Go later receives the
additional subdirectory tag `bindings/go/v4.0.0-rc.1`.

The source validation graph consumes exact `v4.0.0-rc.1` tags for
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

The repository CI additionally proves the feature matrix, Rocq models,
sanitizers, documentation, diagrams, and all language conformance suites.

## Two-phase workflow

Pushing the tag validates and stages artifacts and creates a checksummed
GitHub prerelease. It cannot publish to an external registry. A manual
dispatch at the tag must choose one `registry` value; branch dispatches fail
the contract job.

```bash
gh workflow run release-bindings.yml \
  --repo vinary-tree/libdictenstein \
  --ref v4.0.0-rc.1 \
  -f registry=validate-only

gh workflow run release-bindings.yml \
  --repo vinary-tree/libdictenstein \
  --ref v4.0.0-rc.1 \
  -f registry=npm
```

`validate-only` enables no registry uploader. The other choices—`npm`,
`crates-io`, `pypi`, `maven-central`, `clojars`, `nuget`, `rubygems`,
`go-module`, `luarocks`, and `opam`—each authorize only their matching
protected job. There is deliberately no publish-all option.

## Artifact evidence

The GitHub prerelease retains portable native archives, Python wheels, the npm
tarball, Maven and Clojure staging files, NuGet and Ruby packages, the
LuaRocks and opam metadata, the Hackage numeric candidate, and one
`SHA256SUMS` manifest. Hackage and fpm remain candidate-only for the RC
because their numeric `4.0.0` spellings cannot distinguish it from the final
release.

For npm, the protected job publishes
`@vinary-tree/libdictenstein@4.0.0-rc.1` with provenance under `next`.
Install the public tarball in a clean directory, exercise dictionary
construction, mutation, iteration, snapshot stability, and deterministic
close, then move the new scoped package's `latest` tag to the RC, remove
`bootstrap`, and deprecate the immutable `0.0.0` reservation artifact.

## Failure discipline

Tags and published versions are immutable. A failed validation run may be
rerun with `registry=validate-only`; a failed registry lane may be retried only
when the registry confirms that version was not accepted. If public bytes are
wrong, repair the source and issue `4.0.0-rc.2`. Never move the source tag,
overwrite a package version, or authorize an unrelated registry to compensate
for a failed lane.

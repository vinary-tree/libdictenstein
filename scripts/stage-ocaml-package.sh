#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: $0 <new-output-directory> [source-tag]" >&2
  exit 2
fi

output=$1
if [ -e "$output" ]; then
  echo "output already exists: $output" >&2
  exit 1
fi

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
source_tag=${2:-v$version}
package="libdictenstein-$version"
source="$output/source/$package"
mkdir -p "$source/include"

cp bindings/ocaml/dune.publish "$source/dune"
cp bindings/ocaml/dune-project "$source/"
cp bindings/ocaml/vinary_tree_libdictenstein.ml "$source/"
cp bindings/ocaml/vinary_tree_libdictenstein.mli "$source/"
cp bindings/ocaml/libdictenstein_stubs.c "$source/"
cp bindings/ocaml/libdictenstein.opam.template \
  "$source/libdictenstein.opam"
cp include/libdictenstein.h "$source/include/"
cp include/vinary_tree_interop.h "$source/include/"
cp bindings/ocaml/include/vinary_tree_ocaml.h "$source/include/"
cp README.md "$source/README.md"
cp LICENSE "$source/LICENSE"

archive="$output/$package.tbz"
tar -cjf "$archive" -C "$output/source" "$package"
cp bindings/ocaml/libdictenstein.opam.template "$output/opam"
read -r checksum _ < <(sha256sum "$archive")
printf '\nurl {\n  src: "https://github.com/vinary-tree/libdictenstein/releases/download/%s/%s.tbz"\n  checksum: "sha256=%s"\n}\n' \
  "$source_tag" "$package" "$checksum" >> "$output/opam"

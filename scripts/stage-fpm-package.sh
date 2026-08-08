#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <new-output-directory>" >&2
  exit 2
fi

output=$1
if [ -e "$output" ]; then
  echo "output already exists: $output" >&2
  exit 1
fi

mkdir -p "$output/src"
cp bindings/fortran/fpm.publish.toml "$output/fpm.toml"
cp bindings/fortran/src/vinary_tree_libdictenstein.f90 "$output/src/"
cp bindings/fortran/README.md "$output/README.md"
cp LICENSE "$output/LICENSE"

git -C "$output" init --quiet
git -C "$output" add fpm.toml src README.md LICENSE
git -C "$output" \
  -c user.name="Vinary Tree release automation" \
  -c user.email="dylon.devo@gmail.com" \
  commit --quiet -m "Package libdictenstein"

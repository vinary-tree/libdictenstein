#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <rust-target> <cargo-release-directory> <output-directory>" >&2
  exit 2
fi

target=$1
release_dir=$2
output_dir=$3
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
interop_root=../liblevenshtein-rust

if [ -z "$version" ]; then
  echo "could not read package version from Cargo.toml" >&2
  exit 1
fi
if [ ! -f "$interop_root/vinary-tree-interop/include/vinary_tree_interop.h" ]; then
  echo "missing exact vinary-tree-interop checkout at $interop_root" >&2
  exit 1
fi

package_name="libdictenstein-${version}-${target}"
prefix="${output_dir}/${package_name}"

mkdir -p \
  "${prefix}/bin" \
  "${prefix}/include" \
  "${prefix}/lib/cmake/libdictenstein" \
  "${prefix}/lib/cmake/vinary-tree-interop" \
  "${prefix}/lib/pkgconfig"

cp include/libdictenstein.h include/libdictenstein.hpp "${prefix}/include/"
cp "$interop_root/vinary-tree-interop/include/vinary_tree_interop.h" \
  "${prefix}/include/"
cp cmake/libdictensteinConfig.cmake cmake/libdictensteinConfigVersion.cmake \
  "${prefix}/lib/cmake/libdictenstein/"
cp "$interop_root/cmake/vinary-tree-interopConfig.cmake" \
  "$interop_root/cmake/vinary-tree-interopConfigVersion.cmake" \
  "${prefix}/lib/cmake/vinary-tree-interop/"
cp pkgconfig/libdictenstein.pc "$interop_root/pkgconfig/vinary-tree-interop.pc" \
  "${prefix}/lib/pkgconfig/"
cp LICENSE README.md "${prefix}/"

case "$target" in
  *-pc-windows-msvc)
    shared=$(find "$release_dir" -maxdepth 2 -type f -name 'libdictenstein.dll' -print -quit)
    import_library=$(find "$release_dir" -maxdepth 2 -type f -name 'libdictenstein.dll.lib' -print -quit)
    static_library=$(find "$release_dir" -maxdepth 2 -type f -name 'libdictenstein.lib' -print -quit)
    test -n "$shared" && test -n "$import_library" && test -n "$static_library"
    cp "$shared" "${prefix}/bin/libdictenstein.dll"
    cp "$import_library" "${prefix}/lib/libdictenstein.dll.lib"
    cp "$static_library" "${prefix}/lib/libdictenstein.lib"
    private_libs='-lbcrypt -luserenv -lws2_32 -lntdll -lsynchronization -ladvapi32'
    ;;
  *-apple-darwin)
    shared=$(find "$release_dir" -maxdepth 2 -type f -name 'liblibdictenstein.dylib' -print -quit)
    static_library=$(find "$release_dir" -maxdepth 2 -type f -name 'liblibdictenstein.a' -print -quit)
    test -n "$shared" && test -n "$static_library"
    cp "$shared" "${prefix}/lib/liblibdictenstein.dylib"
    cp "$static_library" "${prefix}/lib/liblibdictenstein.a"
    private_libs='-ldl -lpthread -lm -liconv -framework CoreFoundation -framework Security'
    ;;
  *-linux-gnu)
    shared=$(find "$release_dir" -maxdepth 2 -type f -name 'liblibdictenstein.so' -print -quit)
    static_library=$(find "$release_dir" -maxdepth 2 -type f -name 'liblibdictenstein.a' -print -quit)
    test -n "$shared" && test -n "$static_library"
    cp "$shared" "${prefix}/lib/liblibdictenstein.so"
    cp "$static_library" "${prefix}/lib/liblibdictenstein.a"
    private_libs='-ldl -lpthread -lm'
    ;;
  *)
    echo "unsupported release target: $target" >&2
    exit 1
    ;;
esac

sed -i.bak "s|^Libs.private:.*|Libs.private: ${private_libs}|" \
  "${prefix}/lib/pkgconfig/libdictenstein.pc"
rm -f "${prefix}/lib/pkgconfig/libdictenstein.pc.bak"
tar -czf "${output_dir}/${package_name}.tar.gz" -C "$output_dir" "$package_name"
printf '%s\n' "$prefix"

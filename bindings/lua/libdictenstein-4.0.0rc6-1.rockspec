package = "libdictenstein"
version = "4.0.0rc6-1"
source = { url = "git+https://github.com/vinary-tree/libdictenstein.git", tag = "v4.0.0-rc.6-release.1" }
description = {
  summary = "High-performance dictionaries and trie-maps for approximate string matching",
  detailed = [[
Build mutable, immutable, and persistent dictionaries for fuzzy search.
Choose DynamicDAWG, DoubleArrayTrie, SCDAWG substring indexing, or durable
ARTrie and vocabulary stores. Iterate immutable snapshots, preserve optional
u64 values, and combine dictionaries with union, intersection, and difference.
Requires a matching native SDK and Lua 5.4.

Lua quickstart and complete versioned API reference:
https://vinary-tree.github.io/libdictenstein/4.0.0-rc.6/lua/
]],
  homepage = "https://vinary-tree.github.io/libdictenstein/4.0.0-rc.6/lua/",
  license = "Apache-2.0"
}
dependencies = { "lua >= 5.4" }
external_dependencies = {
  LIBDICTENSTEIN = { header = "libdictenstein.h", library = "libdictenstein" }
}
build = {
  type = "builtin",
  modules = {
    ["vinary_tree.libdictenstein"] = {
      sources = { "bindings/lua/src/libdictenstein_lua.c" },
      incdirs = { "$(LIBDICTENSTEIN_INCDIR)", "include", "bindings/lua/include" },
      libraries = { "libdictenstein" },
      libdirs = { "$(LIBDICTENSTEIN_LIBDIR)" }
    }
  }
}

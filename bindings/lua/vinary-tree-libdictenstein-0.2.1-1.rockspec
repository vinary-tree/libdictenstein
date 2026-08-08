package = "vinary-tree-libdictenstein"
version = "0.2.1-1"
source = { url = "git+https://github.com/vinary-tree/libdictenstein.git", tag = "v0.2.1" }
description = { summary = "Lua bindings for Vinary Tree dictionaries", license = "Apache-2.0" }
dependencies = { "lua >= 5.4" }
build = {
  type = "builtin",
  modules = {
    ["vinary_tree.libdictenstein"] = {
      sources = { "bindings/lua/src/libdictenstein_lua.c" },
      incdirs = { "include", "bindings/lua/include" },
      libraries = { "libdictenstein" },
      libdirs = { "target/release" }
    }
  }
}

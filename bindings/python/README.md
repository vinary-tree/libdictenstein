# libdictenstein for Python

Idiomatic Python CRUD facades for libdictenstein-owned dictionaries. A
`DynamicDawg` exposes the shared `DictionaryResource` protocol and can be passed
directly to `liblevenshtein.Transducer`; the handoff retains two machine words
and performs no serialization or term copying.

Wheels bundle the project native library. Set `LIBDICTENSTEIN_LIBRARY` only for
development against an explicit build.

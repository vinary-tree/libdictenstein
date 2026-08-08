# Vinary Tree libdictenstein for Fortran

This Fortran 2018 package provides DynamicDAWG CRUD and contiguous batch
insertion, immutable DoubleArrayTrie construction, SCDAWG substring operations,
persistent ARTrie CRUD/checkpoint/reopen, persistent vocabulary reverse lookup,
and the shared retained `vt_resource` used by liblevenshtein.

The fpm package is `vinary-tree-libdictenstein`. Link against
`libdictenstein`; published CMake packages support shared or static linkage.

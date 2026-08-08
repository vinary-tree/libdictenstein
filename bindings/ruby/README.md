# Vinary Tree libdictenstein for Ruby

The gem exposes full DynamicDAWG CRUD, immutable DoubleArrayTrie construction,
SCDAWG substring search, persistent ARTrie CRUD/checkpoint/reopen, and persistent
vocabulary reverse lookup. Every object implements `with_resource`, allowing an
independently packaged liblevenshtein transducer to retain it in O(1).

Calls acquire only a short lifetime lease; operations on the same dictionary
are not serialized. The project-owned native resource advertises parallel and
reentrant access automatically.

package io.vinarytree.libdictenstein;

/** Conflict policy for optional u64 values attached to keys in both inputs. */
public enum ValueMerge {
    FIRST(1),
    LAST(2),
    LATTICE_JOIN(3),
    LATTICE_MEET(4);

    final int nativeValue;

    ValueMerge(int nativeValue) { this.nativeValue = nativeValue; }
}

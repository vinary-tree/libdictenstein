package io.vinarytree.libdictenstein;

/** Exact key-set operation evaluated over immutable input revisions. */
public enum AlgebraOperation {
    UNION(1),
    INTERSECTION(2),
    DIFFERENCE(3),
    SYMMETRIC_DIFFERENCE(4);

    final int nativeValue;

    AlgebraOperation(int nativeValue) { this.nativeValue = nativeValue; }
}

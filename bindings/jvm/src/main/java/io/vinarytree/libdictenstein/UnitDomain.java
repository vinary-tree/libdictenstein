package io.vinarytree.libdictenstein;

/** Edge-label domain of a concrete dictionary. */
public enum UnitDomain {
    /** Arbitrary bytes. */
    BYTE(1),
    /** Unicode scalar values decoded from UTF-8. */
    UNICODE_SCALAR(2),
    /** Unsigned 64-bit token bit patterns. */
    U64(3);

    final int nativeValue;

    UnitDomain(int nativeValue) {
        this.nativeValue = nativeValue;
    }
}

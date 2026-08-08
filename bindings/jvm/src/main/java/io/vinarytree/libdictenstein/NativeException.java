package io.vinarytree.libdictenstein;

/** Error reported by libdictenstein's stable native ABI. */
public final class NativeException extends RuntimeException {
    private final int status;

    NativeException(int status, String message) {
        super(message);
        this.status = status;
    }

    /** Numeric stable ABI status. */
    public int status() {
        return status;
    }
}

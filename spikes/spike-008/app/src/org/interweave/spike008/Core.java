// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.spike008;

/** The production human store, through harness/src/jni.rs: a path in, a JSON census out. */
final class Core {
    static {
        System.loadLibrary("spike008");
    }

    private Core() {}

    static native byte[] seed(byte[] path);

    static native byte[] transitions(byte[] path);

    static native byte[] census(byte[] path);

    static native byte[] redeliver(byte[] path);

    static native byte[] fill(byte[] path);

    static native byte[] forensic(byte[] path);
}

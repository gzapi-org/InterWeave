// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.spike009;

/** The Rust core (harness/src/jni.rs): the envelope's bytes, never the cipher. */
final class Core {
    static {
        System.loadLibrary("spike009");
    }

    private Core() {}

    /** Refusal codes, harness/src/framing.rs {@code Refused}. */
    static String refusal(int code) {
        switch (code) {
            case 0: return "ok";
            case 1: return "Length";
            case 2: return "Magic";
            case 3: return "Version";
            case 4: return "Policy";
            case 6: return "WrongIdentity";
            default: return "code" + code;
        }
    }

    static native byte[] peerOf(byte[] seed);

    static native byte[] aad(int policy, byte[] peer);

    static native byte[] frame(int policy, byte[] iv, byte[] sealed);

    static native int check(byte[] envelope);

    static native byte[] part(byte[] envelope, int which);

    static native int verify(byte[] seed, byte[] peer);

    /** The PeerId a typed phrase restores, or null when the production parse refuses it. */
    static native byte[] peerOfPhrase(byte[] words);
}

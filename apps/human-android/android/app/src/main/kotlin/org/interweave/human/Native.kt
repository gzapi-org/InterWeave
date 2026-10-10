// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.human

/**
 * The native library's entry points (`apps/human-android/src/jni.rs`).
 * Every call that BLOCKS is made off the main thread; the Service runs
 * start and stop on one executor, so they never overlap.
 */
internal object Native {
    init {
        System.loadLibrary("interweave_human_android")
    }

    /** The profile's effective availability: the runtime is bound to the app. */
    const val FOREGROUND_ONLY = 0

    /** The profile's effective availability: the person chose Stay reachable. */
    const val STAY_REACHABLE = 1

    /**
     * Start the runtime, the store and the facade under [appDataDir], the
     * app's data directory. BLOCKS. Answers the effective availability,
     * [FOREGROUND_ONLY] or [STAY_REACHABLE], or a negative refusal: the
     * reason is in the log, and the window shows the service as not
     * running.
     */
    @JvmStatic external fun start(appDataDir: String): Int

    /**
     * Wait until the runtime is asked to stop. BLOCKS. 1 when asked, 0
     * when nothing runs.
     */
    @JvmStatic external fun waitEnded(): Int

    /** Stop the runtime, its store and its facade. BLOCKS for the grace. */
    @JvmStatic external fun stop(): Boolean

    /**
     * Wait at most [timeoutMs] for the count of messages no window has
     * read to change: the new count, 0 meaning withdraw the notice, or -1
     * when it did not change. BLOCKS.
     */
    @JvmStatic external fun waitNotice(timeoutMs: Long): Int

    /** A person-facing text by its key ([Text]), with [count] filled in. */
    @JvmStatic external fun text(key: Int, count: Int): String
}

/** The keys of the texts the platform shows, as `jni.rs` maps them. */
internal object Text {
    const val REACHABLE_CHANNEL = 0
    const val REACHABLE_TITLE = 1
    const val REACHABLE_BODY = 2
    const val MESSAGES_CHANNEL = 3
    const val MESSAGES_WAITING = 4
}

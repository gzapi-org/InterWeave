// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.human

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Binder
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Log
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * The network service: it owns the embedded runtime, the store and the
 * session's endpoint lease (human-client-android.md, "Service ownership
 * and local session"). An Activity destroyed or rotated keeps them; the
 * Service's end releases them.
 *
 * The Activity binds it while it is shown. In foreground-only mode that
 * binding is all that keeps it, so the runtime stops when the app leaves
 * the screen. When the profile's effective availability is Stay
 * reachable, the Service also starts itself as a remoteMessaging
 * foreground service with its notification (ADR-0041), and outlives the
 * Activity until the person turns it off or the platform stops it.
 */
class NetworkService : Service() {
    private val binder = Binder()
    private val main = Handler(Looper.getMainLooper())
    @Volatile private var alive = false
    // UNANSWERED until the start answers: no answer, refusal or mode, is
    // ever this value (review of #255: -1 was also NO_IDENTITY).
    @Volatile private var mode = UNANSWERED

    override fun onCreate() {
        super.onCreate()
        alive = true
        channels()
        lifecycle.execute {
            // A start that throws counts as a refusal: it must still end
            // the foreground state below, not leave it to nobody.
            val answer =
                try {
                    Native.start(dataDir.absolutePath)
                } catch (e: RuntimeException) {
                    Log.e(TAG, "start threw", e)
                    START_THREW
                }
            mode = answer
            Log.i(TAG, "start answered $answer")
            if (answer == Native.STAY_REACHABLE) {
                // Started, so that it outlives the Activity's binding.
                startForegroundService(Intent(this, NetworkService::class.java))
            } else {
                // Not Stay reachable, or nothing runs: no foreground service
                // and no "Stay reachable is on" -- a sticky restart posts it
                // before the answer is known (review F1). A bound Activity
                // keeps the Service; nothing else does.
                main.post { leaveForeground() }
            }
            if (answer >= 0) {
                Thread({ awaitEnd() }, "interweave-end").start()
                Thread({ relayNotices() }, "interweave-notices").start()
            }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // A start command comes only for Stay reachable; the notification
        // goes up within the platform's limit for a started foreground
        // service, whatever the runtime is doing.
        startForeground(
            ONGOING_ID,
            ongoing(),
            ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING,
        )
        // The start may have answered already, before this command reached
        // the main thread: its withdrawal then ran first and found nothing
        // to withdraw, so withdraw here (review of #255, F1's ordering).
        // The platform requires startForeground once a foreground start was
        // asked, so it goes up first either way.
        val answer = mode
        if (answer != UNANSWERED && answer != Native.STAY_REACHABLE) {
            leaveForeground()
            return START_NOT_STICKY
        }
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onDestroy() {
        alive = false
        // Off the main thread, and after any start still running: stop
        // waits for the grace, and a start after it finds the lock free.
        lifecycle.execute { Log.i(TAG, "stop answered ${Native.stop()}") }
        super.onDestroy()
    }

    /** Withdraw the Stay-reachable notification and the started state. */
    private fun leaveForeground() {
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    /** The runtime was asked to stop (its admin port, or a stop): end. */
    private fun awaitEnd() {
        if (Native.waitEnded() == 1 && alive) {
            Log.i(TAG, "the runtime was asked to stop")
            stopSelf()
        }
    }

    /** Post or withdraw the count of messages no window has read. */
    private fun relayNotices() {
        val manager = getSystemService(NotificationManager::class.java)
        while (alive) {
            val count = Native.waitNotice(NOTICE_WAIT_MS)
            when {
                count < 0 -> Unit
                count == 0 -> manager.cancel(MESSAGES_ID)
                else -> manager.notify(MESSAGES_ID, messages(count))
            }
        }
        manager.cancel(MESSAGES_ID)
    }

    private fun channels() {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(
                REACHABLE_CHANNEL,
                Native.text(Text.REACHABLE_CHANNEL, 0),
                NotificationManager.IMPORTANCE_LOW,
            ),
        )
        manager.createNotificationChannel(
            NotificationChannel(
                MESSAGES_CHANNEL,
                Native.text(Text.MESSAGES_CHANNEL, 0),
                NotificationManager.IMPORTANCE_DEFAULT,
            ).apply {
                // A count, never content; private on a locked screen.
                lockscreenVisibility = Notification.VISIBILITY_PRIVATE
            },
        )
    }

    private fun opening(): PendingIntent =
        PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )

    private fun ongoing(): Notification =
        Notification.Builder(this, REACHABLE_CHANNEL)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle(Native.text(Text.REACHABLE_TITLE, 0))
            .setContentText(Native.text(Text.REACHABLE_BODY, 0))
            .setContentIntent(opening())
            .setOngoing(true)
            .build()

    private fun messages(count: Int): Notification =
        Notification.Builder(this, MESSAGES_CHANNEL)
            .setSmallIcon(android.R.drawable.stat_notify_chat)
            .setContentTitle(Native.text(Text.MESSAGES_WAITING, count))
            .setContentIntent(opening())
            .setAutoCancel(true)
            .setVisibility(Notification.VISIBILITY_PRIVATE)
            .build()

    companion object {
        private const val TAG = "interweave"
        private const val REACHABLE_CHANNEL = "reachable"
        private const val MESSAGES_CHANNEL = "messages"
        private const val ONGOING_ID = 1
        private const val MESSAGES_ID = 2
        private const val NOTICE_WAIT_MS = 1000L

        /** `mode` before the start answers; never an answer. */
        private const val UNANSWERED = Int.MIN_VALUE

        /** A start that threw, read as a refusal. */
        private const val START_THREW = Int.MIN_VALUE + 1

        /** Start and stop, in order, one at a time, for the process. */
        private val lifecycle: ExecutorService = Executors.newSingleThreadExecutor()
    }
}

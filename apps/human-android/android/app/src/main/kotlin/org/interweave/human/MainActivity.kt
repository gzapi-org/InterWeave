// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.human

import android.Manifest
import android.app.NativeActivity
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.IBinder

/**
 * The window: the native library's `android_main` draws the views and
 * attaches them to the network service's session. The Activity keeps
 * the service bound while it is shown, which is all foreground-only mode
 * runs on.
 */
class MainActivity : NativeActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // API 33+: a notification is posted only with the person's leave.
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
    }

    override fun onStart() {
        super.onStart()
        if (!bound) {
            bound = applicationContext.bindService(
                Intent(applicationContext, NetworkService::class.java),
                connection,
                Context.BIND_AUTO_CREATE,
            )
        }
    }

    override fun onStop() {
        // A recreation for a configuration change keeps the binding: the
        // next instance finds it held, and foreground-only mode's runtime
        // and lease survive it (review F2). Any other stop lets go.
        if (bound && !isChangingConfigurations) {
            applicationContext.unbindService(connection)
            bound = false
        }
        super.onStop()
    }

    companion object {
        // The binding is the application's, not an Activity instance's,
        // so it can outlive one instance and pass to the next.
        private var bound = false
        private val connection =
            object : ServiceConnection {
                override fun onServiceConnected(name: ComponentName?, service: IBinder?) = Unit

                override fun onServiceDisconnected(name: ComponentName?) = Unit
            }
    }
}

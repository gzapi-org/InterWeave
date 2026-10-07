// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.spike008;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.net.ConnectivityManager;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.IBinder;

/**
 * The foreground service standing in for the stay-reachable runtime. It
 * holds no network session: what is measured is whether and when Android
 * keeps, kills and restarts it, and which network changes it is told of.
 * START_STICKY, as a stay-reachable messaging service would ask, so the
 * trace shows whether the system honours it.
 */
public final class Keeper extends Service {
    static final String CHANNEL = "keeper";
    static final long HEARTBEAT_MS = 30_000;

    private HandlerThread thread;
    private Handler handler;
    private ConnectivityManager.NetworkCallback callback;
    private long beats;

    @Override
    public void onCreate() {
        super.onCreate();
        Trace.trace(this, "service-created", "");
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel(CHANNEL, "Harness", NotificationManager.IMPORTANCE_LOW));
        Notification n = new Notification.Builder(this, CHANNEL)
                .setContentTitle("SPIKE-008 harness")
                .setContentText("running")
                .setSmallIcon(android.R.drawable.stat_notify_sync)
                .setOngoing(true)
                .build();
        try {
            startForeground(1, n);
            Trace.trace(this, "foreground-started", "type=" + getForegroundServiceType());
        } catch (RuntimeException e) {
            Trace.trace(this, "foreground-refused", e.getClass().getName() + ": " + e.getMessage());
        }
        thread = new HandlerThread("keeper");
        thread.start();
        handler = new Handler(thread.getLooper());
        handler.postDelayed(this::beat, HEARTBEAT_MS);
        callback = new ConnectivityManager.NetworkCallback() {
            @Override public void onAvailable(Network net) { Trace.trace(Keeper.this, "net-available", String.valueOf(net)); }
            @Override public void onLost(Network net) { Trace.trace(Keeper.this, "net-lost", String.valueOf(net)); }
            @Override public void onCapabilitiesChanged(Network net, NetworkCapabilities caps) {
                String kind = caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) ? "wifi"
                        : caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) ? "cellular" : "other";
                Trace.trace(Keeper.this, "net-capabilities", net + " " + kind
                        + " validated=" + caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED));
            }
            @Override public void onLinkPropertiesChanged(Network net, LinkProperties lp) {
                Trace.trace(Keeper.this, "net-link", net + " addresses=" + lp.getLinkAddresses().size());
            }
        };
        getSystemService(ConnectivityManager.class).registerDefaultNetworkCallback(callback, handler);
    }

    private void beat() {
        beats++;
        Trace.trace(this, "heartbeat", "n=" + beats);
        handler.postDelayed(this::beat, HEARTBEAT_MS);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        Trace.trace(this, "start-command", "intent=" + (intent != null) + " flags=" + flags + " id=" + startId);
        return START_STICKY;
    }

    @Override
    public void onTaskRemoved(Intent root) {
        Trace.trace(this, "task-removed", "");
    }

    @Override
    public void onTrimMemory(int level) {
        Trace.trace(this, "trim-memory", "level=" + level);
    }

    @Override
    public void onLowMemory() {
        Trace.trace(this, "low-memory", "");
    }

    @Override
    public void onDestroy() {
        Trace.trace(this, "service-destroyed", "beats=" + beats);
        try {
            getSystemService(ConnectivityManager.class).unregisterNetworkCallback(callback);
        } catch (RuntimeException ignored) {
            // already gone: the trace line above is what matters
        }
        thread.quitSafely();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}

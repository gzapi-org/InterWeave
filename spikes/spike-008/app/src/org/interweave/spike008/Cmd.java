// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.spike008;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.system.Os;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;

/**
 * SPIKE-008's shell commands:
 * {@code am broadcast -n org.interweave.spike008/.Cmd --es cmd <cmd>}.
 * Guarded by DUMP (the shell and the system). Results go to files/results.jsonl.
 *
 * <ul>
 * <li>layout: write the files the backup rules must exclude -- identity/,
 * config/, recovery-tmp/ -- and the included control, marker/control.txt.
 * Every byte is a TEST-ONLY label, no key material.</li>
 * <li>files: which of those paths exist, with sizes.</li>
 * <li>seed, transitions, census: the production human store in files/human/ (0700).</li>
 * <li>redeliver: U1 (read, not kept) delivered again, and a new message C1 as the control.</li>
 * <li>fill: a store of its own in files/human-full/ under a page quota, filled until it refuses.</li>
 * <li>die: this process SIGKILLs itself, standing in for a low-memory kill.</li>
 * </ul>
 */
public final class Cmd extends BroadcastReceiver {
    static final String[] PATHS = {
        "identity/wrapped.env", "config/profile.json", "recovery-tmp/pending.txt",
        "human/human.sqlite", "marker/control.txt",
    };

    @Override
    public void onReceive(Context context, Intent intent) {
        final PendingResult pending = goAsync();
        final Context app = context.getApplicationContext();
        final String cmd = String.valueOf(intent.getStringExtra("cmd"));
        new Thread(() -> {
            try {
                run(app, cmd);
            } catch (Throwable t) {
                Trace.result(app, cmd, "error " + t.getClass().getSimpleName() + ": " + t.getMessage());
            } finally {
                pending.finish();
            }
        }).start();
    }

    static File store(Context c) throws Exception {
        File dir = new File(c.getFilesDir(), "human");
        if (!dir.isDirectory() && !dir.mkdirs()) {
            throw new IllegalStateException("cannot create " + dir);
        }
        Os.chmod(dir.getPath(), 0700);
        return new File(dir, "human.sqlite");
    }

    static void write(Context c, String rel, String text) throws Exception {
        File f = new File(c.getFilesDir(), rel);
        f.getParentFile().mkdirs();
        Files.write(f.toPath(), text.getBytes(StandardCharsets.UTF_8));
    }

    static void run(Context c, String cmd) throws Exception {
        switch (cmd) {
            case "layout":
                write(c, "identity/wrapped.env", "TEST-ONLY placeholder for the wrapped identity envelope");
                write(c, "config/profile.json", "{\"test_only\":true}");
                write(c, "recovery-tmp/pending.txt", "TEST-ONLY recovery scratch");
                write(c, "marker/control.txt", "TEST-ONLY control: the backup rules leave this included");
                Trace.result(c, "layout", "written");
                break;
            case "files": {
                StringBuilder s = new StringBuilder("{");
                for (String p : PATHS) {
                    File f = new File(c.getFilesDir(), p);
                    if (s.length() > 1) {
                        s.append(',');
                    }
                    s.append(Trace.json(p)).append(':').append(f.exists() ? f.length() : -1);
                }
                Trace.result(c, "files", s.append('}').toString());
                break;
            }
            case "seed":
                Trace.result(c, "seed", new String(Core.seed(path(c)), StandardCharsets.UTF_8));
                break;
            case "transitions":
                Trace.result(c, "transitions", new String(Core.transitions(path(c)), StandardCharsets.UTF_8));
                break;
            case "census":
                Trace.result(c, "census", new String(Core.census(path(c)), StandardCharsets.UTF_8));
                break;
            case "redeliver":
                Trace.result(c, "redeliver", new String(Core.redeliver(path(c)), StandardCharsets.UTF_8));
                break;
            case "fill": {
                // A store of its own, so the quota run never touches the S1 store.
                File dir = new File(c.getFilesDir(), "human-full");
                if (!dir.isDirectory() && !dir.mkdirs()) {
                    throw new IllegalStateException("cannot create " + dir);
                }
                Os.chmod(dir.getPath(), 0700);
                byte[] p = new File(dir, "human.sqlite").getPath().getBytes(StandardCharsets.UTF_8);
                Trace.result(c, "fill", new String(Core.fill(p), StandardCharsets.UTF_8));
                break;
            }
            case "die":
                // L4's stand-in for a low-memory kill: the shell cannot signal
                // another app's process, and lmkd's kill is a SIGKILL, which
                // killProcess sends to this process.
                Trace.result(c, "die", "pid=" + android.os.Process.myPid());
                android.os.Process.killProcess(android.os.Process.myPid());
                break;
            default:
                Trace.result(c, cmd, "unknown command");
        }
    }

    static byte[] path(Context c) throws Exception {
        return store(c).getPath().getBytes(StandardCharsets.UTF_8);
    }
}

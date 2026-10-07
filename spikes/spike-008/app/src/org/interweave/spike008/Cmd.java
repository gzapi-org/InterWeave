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
 * <li>forensic: a store of its own in files/human-forensic/, searched byte for byte for a released message.</li>
 * <li>bench: the cost of RETENTION.md 8's deletion rule -- deletes timed with and without
 * secure_delete and a WAL truncate per delete, SQLite as the store runs it.</li>
 * <li>dirmodes: the modes of files/ and the store's directory, and the store refusing one made 0771.</li>
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

    /**
     * The store's path. The directory is NOT made here: HumanStore::open
     * creates a missing parent owner-only itself and refuses, never
     * tightens, one that is broader (crates/human/store), so a client that
     * chmods first would hide an exposure the store is built to refuse.
     */
    static File store(Context c, String dir) {
        return new File(new File(c.getFilesDir(), dir), "human.sqlite");
    }

    static String mode(File f) {
        try {
            return String.format("%04o", Os.stat(f.getPath()).st_mode & 07777);
        } catch (Exception e) {
            return "absent";
        }
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
            case "fill":
                // A store of its own, so the quota run never touches the S1 store.
                Trace.result(c, "fill", new String(Core.fill(bytes(store(c, "human-full"))), StandardCharsets.UTF_8));
                break;
            case "forensic":
                Trace.result(c, "forensic", new String(Core.forensic(bytes(store(c, "human-forensic"))), StandardCharsets.UTF_8));
                break;
            case "bench": {
                // A directory of its own, made here: it holds only the bench's
                // scratch databases, never the store's content.
                File dir = new File(c.getFilesDir(), "bench");
                if (!dir.isDirectory() && !dir.mkdirs()) {
                    throw new IllegalStateException("cannot create " + dir);
                }
                Trace.result(c, "bench", new String(Core.bench(bytes(dir)), StandardCharsets.UTF_8));
                break;
            }
            case "dirmodes": {
                // files/ itself, the store-created files/human/, and a
                // directory made broader than owner-only before the store
                // opens it, which the store must refuse rather than tighten.
                File broad = new File(c.getFilesDir(), "human-broad");
                if (!broad.isDirectory() && !broad.mkdirs()) {
                    throw new IllegalStateException("cannot create " + broad);
                }
                Os.chmod(broad.getPath(), 0771);
                String opened = new String(Core.census(bytes(store(c, "human-broad"))), StandardCharsets.UTF_8);
                Trace.result(c, "dirmodes", "{\"files\":\"" + mode(c.getFilesDir())
                        + "\",\"files/human\":\"" + mode(new File(c.getFilesDir(), "human"))
                        + "\",\"files/human-broad\":\"" + mode(broad)
                        + "\",\"open_broad\":" + opened + "}");
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

    static byte[] path(Context c) {
        return bytes(store(c, "human"));
    }

    static byte[] bytes(File f) {
        return f.getPath().getBytes(StandardCharsets.UTF_8);
    }
}

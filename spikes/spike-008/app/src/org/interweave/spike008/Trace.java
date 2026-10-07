// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.spike008;

import android.content.Context;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;

/**
 * One JSON line per event in files/{trace,results}.jsonl, read back with
 * run-as. Every line carries wall time, uptime and the pid, so a process
 * restart and a reboot are visible in the trace itself.
 */
final class Trace {
    private Trace() {}

    static void trace(Context c, String event, String detail) {
        write(c, "trace.jsonl", event, detail);
    }

    static void result(Context c, String event, String detail) {
        write(c, "results.jsonl", event, detail);
    }

    private static synchronized void write(Context c, String file, String event, String detail) {
        String line = "{\"t\":" + System.currentTimeMillis()
                + ",\"up\":" + SystemClock.elapsedRealtime()
                + ",\"pid\":" + Process.myPid()
                + ",\"event\":" + json(event)
                + ",\"detail\":" + (detail != null && detail.startsWith("{") ? detail : json(detail)) + "}";
        Log.i("SPIKE008", line);
        try (FileOutputStream out = new FileOutputStream(new File(c.getFilesDir(), file), true)) {
            out.write((line + "\n").getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) {
            Log.e("SPIKE008", "not written: " + e);
        }
    }

    static String json(String s) {
        if (s == null) {
            return "null";
        }
        StringBuilder out = new StringBuilder("\"");
        for (char ch : s.toCharArray()) {
            if (ch == '"' || ch == '\\') {
                out.append('\\').append(ch);
            } else if (ch < 0x20) {
                out.append(String.format("\\u%04x", (int) ch));
            } else {
                out.append(ch);
            }
        }
        return out.append('"').toString();
    }
}

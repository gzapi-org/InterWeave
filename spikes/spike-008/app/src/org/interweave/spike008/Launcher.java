// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.spike008;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.widget.TextView;

/**
 * The only user-visible start path: opening the app starts the foreground
 * service. With {@code --ez recovery true} it opens the recovery screen.
 * Recovery is not exported, but this exported Launcher is a trampoline to
 * it: ANY app can send it that extra. That is a harness convenience only,
 * and a production client must not route recovery through an exported
 * entry point.
 */
public final class Launcher extends Activity {
    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        TextView t = new TextView(this);
        t.setText("SPIKE-008 harness");
        t.setPadding(48, 96, 48, 48);
        setContentView(t);
        Trace.trace(this, "launcher-created", "");
        startForegroundService(new Intent(this, Keeper.class));
        if (getIntent().getBooleanExtra("recovery", false)) {
            // A new task rooted at Recovery: its own affinity makes it one.
            startActivity(new Intent(this, Recovery.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
        }
    }
}

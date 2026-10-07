// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.spike008;

import android.app.Activity;
import android.graphics.Color;
import android.os.Bundle;
import android.view.WindowManager;
import android.widget.TextView;

/**
 * The recovery screen under test: FLAG_SECURE set before any content is
 * drawn, excluded from recents in the manifest, not exported. It shows a
 * TEST-ONLY placeholder line, never a phrase.
 */
public final class Recovery extends Activity {
    @Override
    protected void onCreate(Bundle saved) {
        getWindow().setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE);
        super.onCreate(saved);
        TextView t = new TextView(this);
        t.setText("TEST-ONLY RECOVERY SCREEN\nplaceholder words would be here");
        t.setTextSize(28);
        t.setTextColor(Color.BLACK);
        t.setBackgroundColor(Color.WHITE);
        t.setPadding(48, 160, 48, 48);
        setContentView(t);
        Trace.trace(this, "recovery-shown", "");
    }
}

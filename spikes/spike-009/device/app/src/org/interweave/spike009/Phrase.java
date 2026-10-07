// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.spike009;

import android.app.Activity;
import android.os.Bundle;
import android.text.InputType;
import android.view.ActionMode;
import android.view.Menu;
import android.view.MenuItem;
import android.view.View;
import android.view.WindowManager;
import android.view.inputmethod.EditorInfo;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;

import java.nio.charset.StandardCharsets;
import java.util.Arrays;

/**
 * D7: entering a recovery phrase without leaking it.
 * {@code am start -n org.interweave.spike009/.Phrase [--ez control true]}.
 *
 * <p>SECURE (the default) is the screen under test: FLAG_SECURE before any
 * content, a field that asks the IME for no suggestions and no personalised
 * learning, excluded from autofill, saving no instance state, and offering
 * no copy, cut or paste. CONTROL is the same screen without any of that, so
 * each leak check shows it can see a leak when there is one.
 *
 * <p>The words are checked by the production parse and derivation
 * (Core.peerOfPhrase); only "valid"/"invalid" and whether the PeerId is the
 * fixture's are recorded, never a word, and the field is cleared after.
 * TEST-ONLY: the phrase typed in the run is the public all-zero vector's.
 */
public final class Phrase extends Activity {
    private boolean control;
    private EditText field;

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        control = getIntent().getBooleanExtra("control", false);
        if (!control) {
            getWindow().setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE);
        }
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(32, 64, 32, 32);
        TextView title = new TextView(this);
        title.setText(control ? "CONTROL: unprotected phrase field" : "Enter your 24-word recovery phrase");
        title.setTextSize(20);
        root.addView(title);

        field = new EditText(this);
        field.setSingleLine(true);
        field.setContentDescription("Recovery phrase");
        if (control) {
            field.setInputType(InputType.TYPE_CLASS_TEXT);
        } else {
            field.setInputType(InputType.TYPE_CLASS_TEXT
                    | InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
                    | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
            field.setImeOptions(EditorInfo.IME_ACTION_DONE | EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING);
            root.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS);
            field.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO);
            field.setSaveEnabled(false);
            field.setLongClickable(false);
            ActionMode.Callback none = new ActionMode.Callback() {
                @Override public boolean onCreateActionMode(ActionMode m, Menu menu) { return false; }
                @Override public boolean onPrepareActionMode(ActionMode m, Menu menu) { return false; }
                @Override public boolean onActionItemClicked(ActionMode m, MenuItem i) { return false; }
                @Override public void onDestroyActionMode(ActionMode m) {}
            };
            field.setCustomSelectionActionModeCallback(none);
            field.setCustomInsertionActionModeCallback(none);
        }
        field.setOnEditorActionListener((v, actionId, event) -> {
            check();
            return true;
        });
        root.addView(field);

        Button button = new Button(this);
        button.setText("Restore");
        button.setOnClickListener(v -> check());
        root.addView(button);
        setContentView(root);
        field.requestFocus();
        Cmd.record(this, label("open"), null, "shown", control ? "control" : "secure");
    }

    private String label(String what) {
        return (control ? "phrase-control-" : "phrase-") + what;
    }

    private void check() {
        byte[] words = field.getText().toString().getBytes(StandardCharsets.UTF_8);
        byte[] peer = Core.peerOfPhrase(words);
        Arrays.fill(words, (byte) 0);
        if (!control) {
            field.getText().clear();
        }
        boolean fixture = peer != null && Arrays.equals(peer, Cmd.peer());
        Cmd.record(this, label("check"), null, peer == null ? "invalid" : "valid", "fixture_peer=" + fixture);
    }
}

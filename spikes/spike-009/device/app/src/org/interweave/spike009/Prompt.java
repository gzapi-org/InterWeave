// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.spike009;

import android.app.Activity;
import android.hardware.biometrics.BiometricPrompt;
import android.os.Bundle;
import android.os.CancellationSignal;

import java.nio.file.Files;
import java.util.Arrays;

import javax.crypto.Cipher;
import javax.crypto.spec.GCMParameterSpec;

/**
 * D6b's per-operation key: a wrap or unwrap that the Keystore releases only
 * through a {@code BiometricPrompt} {@code CryptoObject}, one authentication
 * per operation.
 * {@code am start -n org.interweave.spike009/.Prompt --es cmd wrap|unwrap --es alias op}.
 * A person touches the sensor; the outcome is recorded like Cmd's.
 */
public final class Prompt extends Activity {
    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        final String cmd = String.valueOf(getIntent().getStringExtra("cmd"));
        final String alias = String.valueOf(getIntent().getStringExtra("alias"));
        final String label = "prompt-" + cmd;
        final Cipher cipher;
        final byte[] env;
        try {
            cipher = Cipher.getInstance("AES/GCM/NoPadding");
            if ("wrap".equals(cmd)) {
                env = null;
                cipher.init(Cipher.ENCRYPT_MODE, Cmd.key(alias));
            } else {
                env = Files.readAllBytes(Cmd.envFile(this, alias).toPath());
                int code = Core.check(env);
                if (code != 0) {
                    Cmd.record(this, label, alias, Core.refusal(code), "");
                    finish();
                    return;
                }
                cipher.init(Cipher.DECRYPT_MODE, Cmd.key(alias), new GCMParameterSpec(128, Core.part(env, 1)));
            }
        } catch (Exception e) {
            // KeyPermanentlyInvalidatedException surfaces here, at init.
            Cmd.record(this, label, alias, e.getClass().getSimpleName(), String.valueOf(e.getMessage()));
            finish();
            return;
        }
        BiometricPrompt prompt = new BiometricPrompt.Builder(this)
                .setTitle("SPIKE-009 per-operation key")
                .setNegativeButton("Cancel", getMainExecutor(), (d, w) -> {
                    Cmd.record(this, label, alias, "cancelled", "");
                    finish();
                })
                .build();
        prompt.authenticate(new BiometricPrompt.CryptoObject(cipher), new CancellationSignal(), getMainExecutor(),
                new BiometricPrompt.AuthenticationCallback() {
                    @Override
                    public void onAuthenticationSucceeded(BiometricPrompt.AuthenticationResult result) {
                        Cipher c = result.getCryptoObject().getCipher();
                        try {
                            if (env == null) {
                                c.updateAAD(Core.aad(1, Cmd.peer()));
                                byte[] sealed = c.doFinal(Cmd.FIXTURE_SEED);
                                byte[] framed = Core.frame(1, c.getIV(), sealed);
                                Files.write(Cmd.envFile(Prompt.this, alias).toPath(), framed);
                                Cmd.record(Prompt.this, label, alias, "wrapped", "env_len=" + framed.length);
                            } else {
                                c.updateAAD(Core.aad(Core.part(env, 0)[0], Cmd.peer()));
                                byte[] seed = c.doFinal(Core.part(env, 2));
                                int v = Core.verify(seed, Cmd.peer());
                                Arrays.fill(seed, (byte) 0);
                                Cmd.record(Prompt.this, label, alias, v == 0 ? "seed" : Core.refusal(v), "");
                            }
                        } catch (Exception e) {
                            Cmd.record(Prompt.this, label, alias, e.getClass().getSimpleName(), String.valueOf(e.getMessage()));
                        }
                        finish();
                    }

                    @Override
                    public void onAuthenticationError(int code, CharSequence msg) {
                        Cmd.record(Prompt.this, label, alias, "auth-error-" + code, String.valueOf(msg));
                        finish();
                    }
                });
    }
}

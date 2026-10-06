// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
package org.interweave.spike009;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.hardware.biometrics.BiometricManager;
import android.app.KeyguardManager;
import android.os.Build;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyInfo;
import android.security.keystore.KeyProperties;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.security.KeyStore;
import java.util.Map;
import java.util.TreeMap;

import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.SecretKeyFactory;
import javax.crypto.spec.GCMParameterSpec;

/**
 * SPIKE-009's device commands, driven from the shell:
 * {@code am broadcast -n org.interweave.spike009/.Cmd --es cmd <cmd> [--es alias <a>] [--ei timeout <s>]}.
 * Guarded by {@code android.permission.DUMP}, which only the shell and the
 * system hold. Every result is one JSON line appended to files/results.jsonl
 * (read with {@code run-as}); the seed is the TEST-ONLY all-zero fixture
 * vector, and no seed byte is ever written to a result or the log.
 */
public final class Cmd extends BroadcastReceiver {
    static final String TAG = "SPIKE009";
    static final String KS = "AndroidKeyStore";
    /** TEST-ONLY: the public all-zero BIP-39 vector, fixtures/identity/. */
    static final byte[] FIXTURE_SEED = new byte[32];

    @Override
    public void onReceive(Context context, Intent intent) {
        final PendingResult pending = goAsync();
        final Context app = context.getApplicationContext();
        final String cmd = String.valueOf(intent.getStringExtra("cmd"));
        final String alias = intent.getStringExtra("alias");
        final int timeout = intent.getIntExtra("timeout", 60);
        new Thread(() -> {
            try {
                run(app, cmd, alias, timeout);
            } catch (Throwable t) {
                record(app, cmd, alias, "error", t.getClass().getSimpleName() + ": " + t.getMessage());
            } finally {
                pending.finish();
            }
        }).start();
    }

    /** The key mode is the alias's prefix: bg (no user authentication), up (timed), op (per operation). */
    static int policyOf(String alias) {
        return alias != null && alias.startsWith("bg") ? 0 : 1;
    }

    static void run(Context app, String cmd, String alias, int timeout) throws Exception {
        switch (cmd) {
            case "info": info(app); break;
            case "gen": gen(app, alias, timeout, false); break;
            case "gen-strongbox": gen(app, alias, timeout, true); break;
            case "keyinfo": keyinfo(app, alias); break;
            case "wrap": wrap(app, alias); break;
            case "unwrap": unwrapCmd(app, alias, peer()); break;
            case "unwrap-other": unwrapCmd(app, alias, otherPeer()); break;
            case "swap": swap(app, alias); break;
            case "tamper": tamper(app, alias); break;
            case "delete": delete(app, alias); break;
            default: record(app, cmd, alias, "error", "unknown command");
        }
    }

    static byte[] peer() {
        return Core.peerOf(FIXTURE_SEED);
    }

    static byte[] otherPeer() {
        // TEST-ONLY synthetic seed: a second identity, no real key material.
        byte[] seven = new byte[32];
        java.util.Arrays.fill(seven, (byte) 7);
        return Core.peerOf(seven);
    }

    static void info(Context app) {
        PackageManager pm = app.getPackageManager();
        KeyguardManager km = app.getSystemService(KeyguardManager.class);
        BiometricManager bm = app.getSystemService(BiometricManager.class);
        Map<String, Object> m = new TreeMap<>();
        m.put("model", Build.MODEL);
        m.put("sdk", Build.VERSION.SDK_INT);
        m.put("patch", Build.VERSION.SECURITY_PATCH);
        m.put("feature_strongbox", pm.hasSystemFeature(PackageManager.FEATURE_STRONGBOX_KEYSTORE));
        m.put("device_secure", km.isDeviceSecure());
        m.put("biometric_strong", bm.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG));
        m.put("biometric_weak", bm.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_WEAK));
        m.put("fixture_peer", new String(peer(), StandardCharsets.UTF_8));
        recordMap(app, "info", null, m);
    }

    static void gen(Context app, String alias, int timeout, boolean strongbox) throws Exception {
        KeyGenParameterSpec.Builder b = new KeyGenParameterSpec.Builder(alias,
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256);
        if (alias.startsWith("up")) {
            b.setUserAuthenticationRequired(true)
                    .setUserAuthenticationParameters(timeout,
                            KeyProperties.AUTH_DEVICE_CREDENTIAL | KeyProperties.AUTH_BIOMETRIC_STRONG);
        } else if (alias.startsWith("op")) {
            b.setUserAuthenticationRequired(true)
                    .setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG);
        }
        if (strongbox) {
            b.setIsStrongBoxBacked(true);
        }
        KeyGenerator g = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KS);
        try {
            g.init(b.build());
            g.generateKey();
            record(app, strongbox ? "gen-strongbox" : "gen", alias, "ok", "timeout=" + timeout);
        } catch (Exception e) {
            record(app, strongbox ? "gen-strongbox" : "gen", alias, "refused", e.getClass().getName() + ": " + e.getMessage());
        }
    }

    static SecretKey key(String alias) throws Exception {
        KeyStore ks = KeyStore.getInstance(KS);
        ks.load(null);
        return (SecretKey) ks.getKey(alias, null);
    }

    static void keyinfo(Context app, String alias) throws Exception {
        SecretKey k = key(alias);
        if (k == null) {
            record(app, "keyinfo", alias, "absent", "");
            return;
        }
        KeyInfo i = (KeyInfo) SecretKeyFactory.getInstance(k.getAlgorithm(), KS).getKeySpec(k, KeyInfo.class);
        Map<String, Object> m = new TreeMap<>();
        m.put("inside_secure_hardware", i.isInsideSecureHardware());
        m.put("key_size", i.getKeySize());
        m.put("user_auth_required", i.isUserAuthenticationRequired());
        m.put("user_auth_validity_s", i.getUserAuthenticationValidityDurationSeconds());
        m.put("user_auth_type", i.getUserAuthenticationType());
        m.put("invalidated_by_biometric_enrollment", i.isInvalidatedByBiometricEnrollment());
        m.put("user_auth_enforced_by_secure_hardware", i.isUserAuthenticationRequirementEnforcedBySecureHardware());
        m.put("origin", i.getOrigin());
        recordMap(app, "keyinfo", alias, m);
    }

    static File envFile(Context app, String alias) {
        return new File(app.getFilesDir(), alias + ".env");
    }

    static void wrap(Context app, String alias) throws Exception {
        int policy = policyOf(alias);
        Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
        c.init(Cipher.ENCRYPT_MODE, key(alias));
        c.updateAAD(Core.aad(policy, peer()));
        byte[] sealed = c.doFinal(FIXTURE_SEED);
        byte[] iv = c.getIV();
        GCMParameterSpec spec = c.getParameters().getParameterSpec(GCMParameterSpec.class);
        byte[] env = Core.frame(policy, iv, sealed);
        Map<String, Object> m = new TreeMap<>();
        m.put("iv_len", iv.length);
        m.put("tag_bits", spec.getTLen());
        m.put("sealed_len", sealed.length);
        m.put("framed", env != null);
        if (env != null) {
            Files.write(envFile(app, alias).toPath(), env);
            m.put("env_len", env.length);
            m.put("header_hex", hex(env, 0, 6));
        }
        recordMap(app, "wrap", alias, m);
    }

    /** The outcome of one unwrap attempt: "seed" only for the exact seed, re-derived. */
    static String unwrap(String alias, byte[] env, byte[] peer) {
        int code = Core.check(env);
        if (code != 0) {
            return Core.refusal(code);
        }
        try {
            int policy = Core.part(env, 0)[0];
            Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
            c.init(Cipher.DECRYPT_MODE, key(alias), new GCMParameterSpec(128, Core.part(env, 1)));
            c.updateAAD(Core.aad(policy, peer));
            byte[] seed = c.doFinal(Core.part(env, 2));
            int v = Core.verify(seed, peer);
            java.util.Arrays.fill(seed, (byte) 0);
            return v == 0 ? "seed" : Core.refusal(v);
        } catch (Exception e) {
            return e.getClass().getSimpleName();
        }
    }

    static void unwrapCmd(Context app, String alias, byte[] peer) throws Exception {
        byte[] env = Files.readAllBytes(envFile(app, alias).toPath());
        String out = unwrap(alias, env, peer);
        record(app, peer == null ? "unwrap" : (java.util.Arrays.equals(peer, peer()) ? "unwrap" : "unwrap-other"),
                alias, out, "");
    }

    static void swap(Context app, String alias) throws Exception {
        byte[] env = Files.readAllBytes(envFile(app, alias).toPath());
        env[5] = (byte) (env[5] == 0 ? 1 : 0);
        record(app, "swap", alias, unwrap(alias, env, peer()), "valid policy byte swapped");
    }

    static void tamper(Context app, String alias) throws Exception {
        byte[] env = Files.readAllBytes(envFile(app, alias).toPath());
        String control = unwrap(alias, env, peer());
        Map<String, Integer> tally = new TreeMap<>();
        int seeds = 0;
        for (int i = 0; i < env.length; i++) {
            for (int bit = 0; bit < 8; bit++) {
                byte[] t = env.clone();
                t[i] ^= (byte) (1 << bit);
                String out = unwrap(alias, t, peer());
                if ("seed".equals(out)) {
                    seeds++;
                }
                tally.merge(out, 1, Integer::sum);
            }
        }
        Map<String, Object> m = new TreeMap<>();
        m.put("control_intact", control);
        m.put("flips", env.length * 8);
        m.put("seeds_from_flips", seeds);
        m.put("tally", tally);
        recordMap(app, "tamper", alias, m);
    }

    static void delete(Context app, String alias) throws Exception {
        KeyStore ks = KeyStore.getInstance(KS);
        ks.load(null);
        ks.deleteEntry(alias);
        envFile(app, alias).delete();
        record(app, "delete", alias, "ok", "");
    }

    static String hex(byte[] b, int from, int to) {
        StringBuilder s = new StringBuilder();
        for (int i = from; i < to; i++) {
            s.append(String.format("%02x", b[i]));
        }
        return s.toString();
    }

    static void record(Context app, String cmd, String alias, String outcome, String detail) {
        Map<String, Object> m = new TreeMap<>();
        m.put("outcome", outcome);
        m.put("detail", detail);
        recordMap(app, cmd, alias, m);
    }

    static synchronized void recordMap(Context app, String cmd, String alias, Map<String, Object> fields) {
        StringBuilder s = new StringBuilder("{\"t\":").append(System.currentTimeMillis())
                .append(",\"cmd\":").append(json(cmd)).append(",\"alias\":").append(json(alias));
        for (Map.Entry<String, Object> e : fields.entrySet()) {
            s.append(',').append(json(e.getKey())).append(':').append(value(e.getValue()));
        }
        String line = s.append('}').toString();
        Log.i(TAG, line);
        try (FileOutputStream out = new FileOutputStream(new File(app.getFilesDir(), "results.jsonl"), true)) {
            out.write((line + "\n").getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) {
            Log.e(TAG, "result not written: " + e);
        }
    }

    static String value(Object v) {
        if (v == null) {
            return "null";
        }
        if (v instanceof Number || v instanceof Boolean) {
            return v.toString();
        }
        if (v instanceof Map) {
            StringBuilder s = new StringBuilder("{");
            for (Map.Entry<?, ?> e : ((Map<?, ?>) v).entrySet()) {
                if (s.length() > 1) {
                    s.append(',');
                }
                s.append(json(String.valueOf(e.getKey()))).append(':').append(value(e.getValue()));
            }
            return s.append('}').toString();
        }
        return json(v.toString());
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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

plugins {
    alias(libs.plugins.android.application)
}

// The repository's root, where the native library's cargo build runs.
val workspace: File = rootDir.resolve("../../..").canonicalFile
// Where that build's libraries land, one directory per variant and in it
// one per ABI: a variant's own, so a release APK can never package the
// debug library, which carries the stand-in identity (review of #255).
fun rustJniLibs(variant: String): Provider<Directory> =
    layout.buildDirectory.dir("rustJniLibs/${variant.lowercase()}")

android {
    namespace = "org.interweave.human"
    compileSdk = 36
    // The NDK the pins name (tools/host/android/android-toolchain.pins),
    // also the one that strips the native library's debug symbols.
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "org.interweave.human"
        // API 30: the oldest the Stage 17 spikes ran on (SPIKE-008, -009).
        minSdk = 30
        // API 34+ enforces the remoteMessaging foreground-service type
        // (ADR-0041); 36 is Play's floor from 2026-08-31.
        targetSdk = 36
        versionCode = 1
        versionName = "0.0.0"
        ndk {
            // The phones this client is built for (plan section 20).
            abiFilters += "arm64-v8a"
        }
    }

    sourceSets {
        listOf("Debug", "Release").forEach { variant ->
            getByName(variant.lowercase()) {
                jniLibs.directories += rustJniLibs(variant).get().asFile.path
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

// The native library, built by cargo-ndk for every ABI above: the debug
// variant from cargo's dev profile, the release variant from --release.
listOf("Debug", "Release").forEach { variant ->
    val cargo = tasks.register<Exec>("cargoNdk$variant") {
        group = "build"
        description = "Builds the native library for the $variant variant."
        workingDir = workspace
        val sdk = androidComponents.sdkComponents
        doFirst {
            environment("ANDROID_NDK_HOME", sdk.ndkDirectory.get().asFile.path)
            // Slint's Android backend compiles a Java helper against this
            // (it needs API 33's classes; left to itself it takes the
            // lowest platform installed).
            environment(
                "ANDROID_JAR",
                sdk.sdkDirectory.get().asFile.resolve("platforms/android-36/android.jar").path,
            )
        }
        // The release build is optimised; the debug build carries the
        // stand-in profile and identity (crates/human/android-platform's
        // stand_in) until profile-config's provisioning and step 6's
        // Keystore key land, and only it does.
        val profile =
            if (variant == "Release") listOf("--release") else listOf("--features", "dev-stand-ins")
        // --locked: the library is built from the reviewed Cargo.lock, as
        // every other cargo build here is (CI's `cargo check --locked` for
        // this target included); a lockfile the build would rewrite is a
        // failure, not a silent new dependency set in the APK.
        commandLine(
            listOf(
                "cargo", "ndk", "-t", "arm64-v8a", "-P", "30",
                "-o", rustJniLibs(variant).get().asFile.path,
                "build", "--locked", "-p", "interweave-human-android",
            ) + profile,
        )
    }
    tasks.matching { it.name == "merge${variant}JniLibFolders" }.configureEach {
        dependsOn(cargo)
    }
}

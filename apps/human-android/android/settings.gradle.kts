// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
// The Android human client's Gradle build (plan section 20): one app
// module packaging the native library apps/human-android builds.

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    // Only the repositories named here: a module declaring its own is an
    // error, so the dependency graph has one place to read.
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "interweave-human-android"
include(":app")

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

// Every resolved dependency, the plugins' classpath included, is locked
// in a committed lockfile (gradle.lockfile per project,
// buildscript-gradle.lockfile here), which the local OSV.dev scan reads
// (plan section 20, "Dependency hygiene for the Gradle build"). A change
// to the graph is written with --write-locks and reviewed as a diff.
buildscript {
    configurations.classpath {
        resolutionStrategy.activateDependencyLocking()
    }
}

plugins {
    alias(libs.plugins.android.application) apply false
}

allprojects {
    dependencyLocking {
        lockAllConfigurations()
    }
}

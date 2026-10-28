pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "pod"

// `core` is plain JVM Kotlin: the wire types and the pure rules every client
// shares with pod-protocol (ids, feed URL normalization, LWW). Like
// pod-protocol it does no I/O, so its tests run without Android.
include(":core")

// The Android app: Room store, sync engine, playback, downloads and the Compose UI.
include(":app")

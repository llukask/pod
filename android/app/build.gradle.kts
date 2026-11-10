plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.ksp)
}

android {
    namespace = "ltd.kilian.pod"
    compileSdk = 36

    defaultConfig {
        // v2 shares v1's application id, so it replaces v1 on a device. The local
        // database has a new name (`PodDatabase.NAME`), so v1's Room file is never opened.
        applicationId = "ltd.kilian.pod"
        minSdk = 34
        targetSdk = 36
        // Release builds take both from the git tag (release.yml).
        versionCode = providers.gradleProperty("pod.versionCode").map(String::toInt).getOrElse(2)
        versionName = providers.gradleProperty("pod.versionName").getOrElse("2.0")

        // What the sign-in screen prefills; "Change server" edits it.
        buildConfigField("String", "DEFAULT_SERVER_URL", "\"https://pod.kilian.ltd\"")
    }

    // Release builds are signed with the key in GitHub secrets (release.yml).
    // Without these variables, assembleRelease produces an unsigned APK.
    val releaseKeystore = System.getenv("POD_KEYSTORE")
    signingConfigs {
        if (releaseKeystore != null) {
            create("release") {
                storeFile = file(releaseKeystore)
                storePassword = System.getenv("POD_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("POD_KEY_ALIAS")
                keyPassword = System.getenv("POD_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        debug {
            applicationIdSuffix = ".debug"
        }
        release {
            signingConfig = signingConfigs.findByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    testOptions {
        unitTests.isIncludeAndroidResources = true
        unitTests.all {
            // The fixtures shared with pod-protocol and pod-desktop (conformance/README.md).
            it.systemProperty("pod.conformance", rootProject.projectDir.resolve("../conformance").canonicalPath)
            it.inputs.dir(rootProject.projectDir.resolve("../conformance"))
            // Room's exported schemas, which the migration tests build old databases from.
            it.systemProperty("pod.schemas", projectDir.resolve("schemas/ltd.kilian.pod.store.PodDatabase").canonicalPath)
        }
    }
}

// Room writes each schema version here, so migrations are reviewable and testable.
ksp {
    arg("room.schemaLocation", "$projectDir/schemas")
}

dependencies {
    implementation(project(":core"))
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.material.icons.extended)
    implementation(libs.coil.compose)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.okhttp)
    implementation(libs.androidx.work.runtime.ktx)
    implementation(libs.androidx.media3.exoplayer)
    implementation(libs.androidx.media3.session)
    implementation(libs.androidx.lifecycle.process)
    implementation(libs.room.runtime)
    implementation(libs.room.ktx)
    ksp(libs.room.compiler)

    testImplementation(libs.junit4)
    testImplementation(libs.robolectric)
    testImplementation(libs.androidx.test.core)
    testImplementation(libs.kotlinx.coroutines.test)
}

plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

kotlin {
    jvmToolchain(21)
}

dependencies {
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.okhttp)
    testImplementation(platform(libs.junit5.bom))
    testImplementation(libs.junit5.jupiter)
    testRuntimeOnly(libs.junit5.launcher)
}

tasks.test {
    useJUnitPlatform()
    // The fixtures are shared with pod-protocol and pod-desktop and live at
    // the repo root (conformance/README.md).
    systemProperty("pod.conformance", rootProject.projectDir.resolve("../conformance").canonicalPath)
    inputs.dir(rootProject.projectDir.resolve("../conformance"))
    // Generated from pod-protocol; `OpenApiDriftTest` checks the wire types against it.
    systemProperty("pod.openapi", rootProject.projectDir.resolve("../openapi.json").canonicalPath)
    inputs.file(rootProject.projectDir.resolve("../openapi.json"))
}

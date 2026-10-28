package ltd.kilian.pod.core

import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/** Reads a fixture from the repo's `conformance/` directory, which the Gradle test task names. */
fun fixture(name: String): JsonObject {
    val dir = System.getProperty("pod.conformance") ?: error("pod.conformance is set by the Gradle test task")
    return Json.parseToJsonElement(File(dir, name).readText()).jsonObject
}

fun JsonObject.string(key: String): String = getValue(key).jsonPrimitive.content

fun JsonObject.stringOrNull(key: String): String? =
    get(key).takeUnless { it == null || it is JsonNull }?.jsonPrimitive?.content

val JsonElement.name: String get() = jsonObject.string("name")

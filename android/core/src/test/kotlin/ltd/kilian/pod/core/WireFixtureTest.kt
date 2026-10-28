package ltd.kilian.pod.core

import java.io.File
import kotlinx.serialization.KSerializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.DynamicTest
import org.junit.jupiter.api.DynamicTest.dynamicTest
import org.junit.jupiter.api.TestFactory

/**
 * Every sync response and outbox payload in the `sync_*.json` and `client_*.json` fixtures is a
 * real wire body (conformance/README.md), so each must decode and encode back to the same JSON.
 */
class WireFixtureTest {
    @TestFactory
    fun fixtureBodiesRoundTrip(): List<DynamicTest> {
        val dir = File(System.getProperty("pod.conformance") ?: error("pod.conformance is set by the Gradle test task"))
        val files = dir.listFiles { f -> f.name.matches(Regex("(sync|client)_.*\\.json")) }!!.sortedBy { it.name }
        return files.flatMap { file ->
            fixture(file.name).getValue("cases").jsonArray.flatMap { case ->
                val row = case.jsonObject
                val responses = listOfNotNull(row["server_response"]) +
                    (row["steps"]?.jsonArray ?: emptyList()).mapNotNull { it.jsonObject["response"] }
                val payloads = listOfNotNull(row["local_before"], row["local_after"])
                    .flatMap { tables -> listOf("outbox", "rejected_mutation").flatMap { tables.jsonObject[it]?.jsonArray ?: emptyList() } }
                    .mapNotNull { it.jsonObject["payload"] }
                responses.mapIndexed { i, body ->
                    dynamicTest("${file.name}: ${case.name} (response ${i + 1})") { roundTrip(SyncResponse.serializer(), body) }
                } + payloads.mapIndexed { i, body ->
                    dynamicTest("${file.name}: ${case.name} (payload ${i + 1})") { roundTrip(Mutation.serializer(), body) }
                }
            }
        }
    }

    private fun <T> roundTrip(serializer: KSerializer<T>, body: JsonElement) {
        val decoded = WireJson.decodeFromJsonElement(serializer, body)
        assertEquals(body, WireJson.encodeToJsonElement(serializer, decoded))
    }
}

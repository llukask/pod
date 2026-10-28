package ltd.kilian.pod.core

import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.DynamicTest
import org.junit.jupiter.api.DynamicTest.dynamicTest
import org.junit.jupiter.api.TestFactory

/**
 * `conformance/lww.json`, the `wins` table. Its `clamp` and `apply` tables are
 * the server's half of spec 4.4; a client never clamps, so it doesn't run them.
 */
class LwwConformanceTest {
    @TestFactory
    fun wins(): List<DynamicTest> = fixture("lww.json").getValue("wins").jsonArray.map { case ->
        val row = case.jsonObject
        dynamicTest(case.name) {
            val old = Stamp.fromColumns(row.stringOrNull("old_at")?.let(Timestamps::parse), row.stringOrNull("old_dev"))
            val new = Stamp(Timestamps.parse(row.string("at")), row.string("dev"))
            assertEquals(row.getValue("wins").jsonPrimitive.boolean, wins(new, old))
        }
    }
}

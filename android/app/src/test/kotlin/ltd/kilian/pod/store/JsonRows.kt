package ltd.kilian.pod.store

import android.database.Cursor
import androidx.sqlite.db.SupportSQLiteDatabase
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.longOrNull
import ltd.kilian.pod.core.Timestamps

// ==============================================================================================
// Local rows in the conformance fixtures' JSON encoding (conformance/README.md)
// ==============================================================================================
//
// The encoding differs from SQLite's in three ways: timestamps are RFC 3339 strings instead of
// epoch ms, flags are booleans instead of 0 / 1, and JSON columns hold the JSON value itself
// instead of its text. The same rules as pod-desktop's `store/json_rows.rs`.

private val FLAG_COLUMNS = setOf("subscribed", "played", "archived", "complete", "metered_ok", "auto_on_metered", "notify")

private val JSON_COLUMNS = setOf("setting.value", "device_setting.value", "outbox.payload", "rejected_mutation.payload")

private fun isTime(column: String) = column.endsWith("_at") || column == "unwanted_since"

private fun isJson(table: String, column: String) = "$table.$column" in JSON_COLUMNS

/** Inserts one fixture row into [table]. */
fun insertJsonRow(db: SupportSQLiteDatabase, table: String, row: JsonObject) {
    val columns = row.keys.toList()
    val values = columns.map { column -> toSql(table, column, row.getValue(column)) }
    db.exec("insert into $table (${columns.joinToString()}) values (${columns.joinToString { "?" }})", *values.toTypedArray())
}

private fun toSql(table: String, column: String, value: JsonElement): Any? = when {
    isJson(table, column) -> value.toString()
    value is JsonNull -> null
    value is JsonPrimitive && value.isString && isTime(column) -> Timestamps.parse(value.content)
    value is JsonPrimitive && value.isString -> value.content
    value is JsonPrimitive && column in FLAG_COLUMNS && value.booleanOrNull != null -> value.boolean
    value is JsonPrimitive -> value.longOrNull ?: value.doubleOrNull ?: error("$table.$column = $value has no SQL form")
    else -> error("$table.$column = $value has no SQL form")
}

/** Every row of every local table, as `{ "<table>": [ { "<column>": ... } ] }`. */
fun dumpJsonRows(db: SupportSQLiteDatabase): Map<String, List<JsonObject>> =
    PodDatabase.TABLES.associateWith { table ->
        db.queryList("select * from $table") { cursor ->
            JsonObject(cursor.columnNames.withIndex().associate { (index, column) -> column to toJson(table, column, cursor, index) })
        }
    }

private fun toJson(table: String, column: String, cursor: Cursor, index: Int): JsonElement = when (cursor.getType(index)) {
    Cursor.FIELD_TYPE_NULL -> JsonNull
    Cursor.FIELD_TYPE_STRING ->
        if (isJson(table, column)) Json.parseToJsonElement(cursor.getString(index)) else JsonPrimitive(cursor.getString(index))
    Cursor.FIELD_TYPE_INTEGER -> when {
        isTime(column) -> JsonPrimitive(Timestamps.format(cursor.getLong(index)))
        column in FLAG_COLUMNS -> JsonPrimitive(cursor.getLong(index) != 0L)
        else -> JsonPrimitive(cursor.getLong(index))
    }
    Cursor.FIELD_TYPE_FLOAT -> JsonPrimitive(cursor.getDouble(index))
    else -> error("$table.$column holds a blob")
}

/** Each table's primary key columns, as the fixtures match rows by them. `account` has one row and no key. */
private val PRIMARY_KEYS = mapOf(
    "podcast" to listOf("id"), "episode" to listOf("id"), "subscription" to listOf("podcast_id"),
    "episode_state" to listOf("episode_id"), "setting" to listOf("key"), "sync_scope" to listOf("scope"),
    "outbox" to listOf("mutation_id"), "account" to emptyList(), "podcast_device" to listOf("podcast_id"),
    "download_wish" to listOf("episode_id", "source"), "download" to listOf("episode_id"),
    "known_size" to listOf("episode_id"), "device_setting" to listOf("key"),
    "rejected_mutation" to listOf("mutation_id"), "artwork" to listOf("url"),
)

/**
 * Compares [actual] with a partial expectation: only the named tables, rows matched one to one
 * by primary key, and only the listed columns. Returns every difference.
 */
fun compareTables(actual: Map<String, List<JsonObject>>, expected: Map<String, List<JsonObject>>): List<String> = buildList {
    for ((table, expectedRows) in expected) {
        val key = PRIMARY_KEYS[table]
        val actualRows = actual[table]
        if (key == null || actualRows == null) {
            add("unknown table \"$table\"")
            continue
        }
        fun keyOf(row: JsonObject): String? =
            if (key.isEmpty()) "(the row)" else key.map { row[it] ?: return null }.joinToString(" ")
        val actualByKey = mutableMapOf<String, JsonObject>()
        for (row in actualRows) {
            val rowKey = keyOf(row) ?: continue
            if (actualByKey.put(rowKey, row) != null) add("$table $rowKey: duplicate primary key")
        }
        val expectedKeys = mutableSetOf<String>()
        for (want in expectedRows) {
            val rowKey = keyOf(want)
            if (rowKey == null) {
                add("$table: expected row $want lacks its key column")
                continue
            }
            expectedKeys += rowKey
            val got = actualByKey[rowKey]
            if (got == null) {
                add("$table $rowKey: row missing")
                continue
            }
            for ((column, value) in want) {
                when (val gotValue = got[column]) {
                    null -> add("$table $rowKey: no column \"$column\"")
                    value -> {}
                    else -> add("$table $rowKey.$column: want $value, got $gotValue")
                }
            }
        }
        actualByKey.keys.filterNot { it in expectedKeys }.forEach { add("$table $it: unexpected row") }
    }
}

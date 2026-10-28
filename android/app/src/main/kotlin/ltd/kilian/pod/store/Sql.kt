package ltd.kilian.pod.store

import android.database.Cursor
import androidx.sqlite.db.SupportSQLiteDatabase
import androidx.sqlite.db.SupportSQLiteProgram

// Plain SQL over the store's connection. The sync steps use it rather than Room DAOs because
// they are ports of pod-desktop's statements, and keeping the SQL identical keeps the two
// clients easy to compare. Room still owns the schema and the live queries the UI reads, and
// `RoomDatabase.runInTransaction` notifies those after every write made here.

/** Runs one statement and returns how many rows it changed. */
fun SupportSQLiteDatabase.exec(sql: String, vararg args: Any?): Int =
    compileStatement(sql).use { statement ->
        statement.bindAll(args)
        statement.executeUpdateDelete()
    }

fun <T> SupportSQLiteDatabase.queryList(sql: String, vararg args: Any?, read: (Cursor) -> T): List<T> =
    query(sql, args.map(::sqlArg).toTypedArray()).use { cursor ->
        buildList { while (cursor.moveToNext()) add(read(cursor)) }
    }

fun <T> SupportSQLiteDatabase.queryOne(sql: String, vararg args: Any?, read: (Cursor) -> T): T? =
    queryList(sql, *args, read = read).firstOrNull()

private fun SupportSQLiteProgram.bindAll(args: Array<out Any?>) =
    args.forEachIndexed { index, arg ->
        when (val value = sqlArg(arg)) {
            null -> bindNull(index + 1)
            is Long -> bindLong(index + 1, value)
            is Double -> bindDouble(index + 1, value)
            is String -> bindString(index + 1, value)
            else -> error("unbindable SQL argument $value")
        }
    }

/** SQLite has integers, reals and text; flags are 0 / 1. */
private fun sqlArg(arg: Any?): Any? = when (arg) {
    null -> null
    is Boolean -> if (arg) 1L else 0L
    is Int -> arg.toLong()
    is Long, is String, is Double -> arg
    else -> error("unsupported SQL argument type ${arg::class}")
}

fun Cursor.string(column: String): String = getString(getColumnIndexOrThrow(column))

fun Cursor.stringOrNull(column: String): String? =
    getColumnIndexOrThrow(column).let { if (isNull(it)) null else getString(it) }

fun Cursor.long(column: String): Long = getLong(getColumnIndexOrThrow(column))

fun Cursor.longOrNull(column: String): Long? =
    getColumnIndexOrThrow(column).let { if (isNull(it)) null else getLong(it) }

fun Cursor.flag(column: String): Boolean = getLong(getColumnIndexOrThrow(column)) != 0L

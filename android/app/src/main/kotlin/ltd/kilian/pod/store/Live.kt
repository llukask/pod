package ltd.kilian.pod.store

import androidx.sqlite.db.SupportSQLiteDatabase
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.map

/**
 * A live query: runs [read] now and again after every committed write to one of [tables], off
 * the main thread. Room's invalidation tracker sees the plain-SQL writes of the sync steps too,
 * because they run inside `runInTransaction`. Bursts of writes conflate into one re-read.
 */
fun <T> PodDatabase.observe(vararg tables: String, read: (SupportSQLiteDatabase) -> T): Flow<T> =
    invalidationTracker.createFlow(*tables)
        .conflate()
        .map { read(openHelper.readableDatabase) }
        .flowOn(Dispatchers.IO)

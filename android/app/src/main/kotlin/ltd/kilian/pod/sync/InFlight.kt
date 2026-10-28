package ltd.kilian.pod.sync

/**
 * The mutation ids of the request being sent (spec 5.2). A local write must not coalesce away a
 * row in this set, because its result is still to come and deletes it by id.
 *
 * Local writes check the set inside their transaction while holding its lock, so it can't
 * change between the coalescing check and the commit.
 */
class InFlight {
    private val ids = mutableSetOf<String>()

    fun set(batch: Collection<String>) = synchronized(this) {
        ids.clear()
        ids.addAll(batch)
    }

    fun clear() = set(emptyList())

    fun <T> withLock(block: (Set<String>) -> T): T = synchronized(this) { block(ids.toSet()) }
}

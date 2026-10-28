package ltd.kilian.pod.core

import java.nio.ByteBuffer
import java.security.MessageDigest
import java.util.UUID

/** The deterministic ids of spec section 1. */
object Ids {
    val NS_PODCAST: UUID = UUID.fromString("6f1d3c2a-9b0e-5e7a-8c41-2d7f0a3b9e01")
    val NS_EPISODE: UUID = UUID.fromString("0c8a7e54-3f21-5b6d-9a10-7e4c2b1d8f02")

    /** UUIDv5 of a normalized feed URL ([NormalizedFeedUrl.normalized]). */
    fun podcastId(normalized: String): String = uuidV5(NS_PODCAST, normalized)

    /**
     * The item's guid, trimmed, or the enclosure URL when the guid is missing or blank. The
     * guid is not otherwise normalized, so case and URL-looking guids are kept.
     */
    fun episodeKey(guid: String?, enclosureUrl: String): String =
        guid?.trim()?.takeIf { it.isNotEmpty() } ?: enclosureUrl

    fun episodeId(podcastId: String, key: String): String = uuidV5(NS_EPISODE, "$podcastId\n$key")

    /** RFC 9562 name-based UUID with SHA-1. The JDK only ships the MD5 variant (v3). */
    private fun uuidV5(namespace: UUID, name: String): String {
        val sha1 = MessageDigest.getInstance("SHA-1")
        sha1.update(
            ByteBuffer.allocate(16)
                .putLong(namespace.mostSignificantBits)
                .putLong(namespace.leastSignificantBits)
                .array()
        )
        val hash = sha1.digest(name.toByteArray(Charsets.UTF_8))
        hash[6] = ((hash[6].toInt() and 0x0f) or 0x50).toByte()
        hash[8] = ((hash[8].toInt() and 0x3f) or 0x80).toByte()
        val bytes = ByteBuffer.wrap(hash, 0, 16)
        return UUID(bytes.long, bytes.long).toString()
    }
}

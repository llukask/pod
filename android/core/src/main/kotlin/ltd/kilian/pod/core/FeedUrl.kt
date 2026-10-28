package ltd.kilian.pod.core

import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

enum class FeedUrlError(val code: String) {
    /** The input doesn't parse as an absolute URL. */
    InvalidUrl("invalid_url"),

    /** It parses, but the scheme isn't `http` or `https` (`feed://` included). */
    UnsupportedScheme("unsupported_scheme"),
}

/**
 * A feed URL that passed normalization (spec section 1). [feedUrl] keeps the user's scheme and
 * is what gets stored and fetched; [normalized] maps `http` to `https` and is what the podcast
 * id hashes. Either may carry a private feed's credentials, so [toString] hides them.
 */
class NormalizedFeedUrl internal constructor(val feedUrl: String, val normalized: String) {
    val podcastId: String get() = Ids.podcastId(normalized)

    override fun toString(): String = "NormalizedFeedUrl(${FeedUrl.host(feedUrl)})"
}

sealed interface FeedUrlResult {
    data class Ok(val url: NormalizedFeedUrl) : FeedUrlResult

    data class Err(val error: FeedUrlError) : FeedUrlResult
}

object FeedUrl {
    /**
     * Normalizes [input] the way pod-protocol's `url`-crate (WHATWG) parser does, then drops
     * the fragment and derives the https form. `conformance/ids.json` is the contract;
     * `conformance/README.md` lists every rule it pins.
     *
     * OkHttp's [HttpUrl] parses the authority (credentials, IDNA host, IPv6, port). The path
     * and query are serialized here, because HttpUrl's encode sets differ from WHATWG's (it
     * encodes `|` and `^` in paths) in a way that can't be undone afterwards: its `%7C` for a
     * literal `|` looks exactly like a `%7C` the user wrote, which must be kept as written.
     */
    fun normalize(input: String): FeedUrlResult {
        // Rust's `str::trim`, then WHATWG's own stripping of C0 controls and spaces at either
        // end and of tab, LF and CR anywhere.
        val text = input.trim().trim { it <= ' ' }.filterNot { it == '\t' || it == '\n' || it == '\r' }

        val colon = text.indexOf(':')
        if (colon <= 0 || !isScheme(text.substring(0, colon))) return FeedUrlResult.Err(FeedUrlError.InvalidUrl)
        val scheme = text.substring(0, colon).lowercase()
        if (scheme != "http" && scheme != "https") return FeedUrlResult.Err(FeedUrlError.UnsupportedScheme)

        // Special schemes take any number of slashes or backslashes before the authority, and
        // the authority runs to the first slash, backslash, `?` or `#`.
        val rest = text.substring(colon + 1).trimStart { it == '/' || it == '\\' }
        val authorityEnd = rest.indexOfFirst { it == '/' || it == '\\' || it == '?' || it == '#' }.let { if (it < 0) rest.length else it }
        val authority = rest.substring(0, authorityEnd)
        val afterAuthority = rest.substring(authorityEnd).substringBefore('#')
        val path = afterAuthority.substringBefore('?')
        val query = if ('?' in afterAuthority) afterAuthority.substringAfter('?') else null

        val parsed = "$scheme://$authority/".toHttpUrlOrNull() ?: return FeedUrlResult.Err(FeedUrlError.InvalidUrl)
        val host = canonicalHost(parsed.host) ?: return FeedUrlResult.Err(FeedUrlError.InvalidUrl)
        val userinfo = when {
            parsed.encodedPassword.isNotEmpty() -> "${parsed.encodedUsername}:${parsed.encodedPassword}@"
            parsed.encodedUsername.isNotEmpty() -> "${parsed.encodedUsername}@"
            else -> ""
        }
        val tail = serializePath(path) + (query?.let { "?" + percentEncode(it, QUERY_SET) } ?: "")

        fun render(scheme: String, port: Int): String {
            val portText = if (port == HttpUrl.defaultPort(scheme)) "" else ":$port"
            return "$scheme://$userinfo$host$portText$tail"
        }

        // HttpUrl fills in the scheme's default port, so an http URL without one reads as 80
        // and must become https's 443, while an explicit :443 on http turns into the default.
        val httpsPort = if (scheme == "http" && parsed.port == 80) 443 else parsed.port
        return FeedUrlResult.Ok(NormalizedFeedUrl(render(scheme, parsed.port), render("https", httpsPort)))
    }

    /** The host of a stored feed URL with a non-default port, for naming an untitled podcast (spec 5.5). */
    fun host(feedUrl: String): String {
        val url = feedUrl.toHttpUrlOrNull() ?: return "?"
        val host = if (':' in url.host) "[${url.host}]" else url.host
        return if (url.port == HttpUrl.defaultPort(url.scheme)) host else "$host:${url.port}"
    }

    // ==========================================================================================
    // WHATWG pieces HttpUrl doesn't share
    // ==========================================================================================

    /** An ASCII letter, then letters, digits, `+`, `-` or `.`. */
    private fun isScheme(text: String): Boolean =
        text[0].isAsciiLetter() && text.all { it.isAsciiLetter() || it in '0'..'9' || it == '+' || it == '-' || it == '.' }

    private fun Char.isAsciiLetter() = this in 'a'..'z' || this in 'A'..'Z'

    /**
     * Brackets IPv6 hosts and rewrites a host whose last label is a number (decimal, `0x` hex
     * or `0`-prefixed octal, in one to four parts) as dotted-decimal IPv4, failing (null) when
     * such a host is out of range. HttpUrl keeps `3232235786` as a domain name.
     */
    private fun canonicalHost(host: String): String? {
        if (':' in host) return "[$host]"
        val parts = host.split('.').let { if (it.size > 1 && it.last().isEmpty()) it.dropLast(1) else it }
        val last = parts.last()
        val endsInNumber = last.isNotEmpty() && (last.all { it in '0'..'9' } ||
            (last.length >= 2 && last[0] == '0' && (last[1] == 'x' || last[1] == 'X') && last.drop(2).all { it.isHexDigit() }))
        if (!endsInNumber) return host
        if (parts.size > 4) return null
        val numbers = parts.map { ipv4Number(it) ?: return null }
        if (numbers.dropLast(1).any { it > 255 }) return null
        if (numbers.last() >= 1L shl (8 * (5 - numbers.size))) return null
        var address = numbers.last()
        numbers.dropLast(1).forEachIndexed { i, n -> address += n shl (8 * (3 - i)) }
        return (3 downTo 0).joinToString(".") { ((address shr (8 * it)) and 0xff).toString() }
    }

    private fun Char.isHexDigit() = this in '0'..'9' || this in 'a'..'f' || this in 'A'..'F'

    private fun ipv4Number(part: String): Long? {
        if (part.isEmpty()) return null
        val (digits, radix) = when {
            part.length >= 2 && part[0] == '0' && (part[1] == 'x' || part[1] == 'X') -> part.drop(2) to 16
            part.length >= 2 && part[0] == '0' -> part.drop(1) to 8
            else -> part to 10
        }
        if (digits.isEmpty()) return 0
        // Anything past 2^32 fails the range check anyway; cap the length so it can't overflow.
        if (digits.length > 12) return Long.MAX_VALUE
        return digits.toLongOrNull(radix)
    }

    /**
     * A special-scheme path: backslashes are separators, `.` and `..` segments (and their
     * `%2e` spellings) are resolved, an empty path is `/`, and [PATH_SET] is percent-encoded.
     */
    private fun serializePath(raw: String): String {
        val parts = raw.drop(if (raw.startsWith('/') || raw.startsWith('\\')) 1 else 0).split('/', '\\')
        val segments = mutableListOf<String>()
        parts.forEachIndexed { i, part ->
            val isLast = i == parts.lastIndex
            when (part.lowercase()) {
                "..", ".%2e", "%2e.", "%2e%2e" -> {
                    segments.removeLastOrNull()
                    if (isLast) segments += ""
                }
                ".", "%2e" -> if (isLast) segments += ""
                else -> segments += percentEncode(part, PATH_SET)
            }
        }
        return "/" + segments.joinToString("/")
    }

    /**
     * The `url` crate's path set: the C0 control set plus these. The current WHATWG standard
     * also encodes `^`; the crate (and so the fixture) doesn't.
     */
    private const val PATH_SET = " \"#<>?`{}"

    /** The special-query set. */
    private const val QUERY_SET = " \"#<>'"

    /**
     * Percent-encodes C0 controls, everything above U+007E (as UTF-8) and [set]. `%` is never
     * encoded, so existing escapes stay as written, invalid ones included.
     */
    private fun percentEncode(text: String, set: String): String = buildString {
        for (byte in text.toByteArray(Charsets.UTF_8)) {
            val b = byte.toInt() and 0xff
            if (b < 0x20 || b > 0x7e || b.toChar() in set) append('%').append("%02X".format(b)) else append(b.toChar())
        }
    }
}

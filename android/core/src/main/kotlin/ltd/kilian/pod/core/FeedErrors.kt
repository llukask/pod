package ltd.kilian.pod.core

/**
 * One short sentence for the user, without a trailing period and never with the URL in it
 * (spec 3.2). The same words as pod-protocol's `FeedError::message`. A code a later server adds
 * gets a generic sentence.
 */
val FeedError.message: String
    get() = when (code) {
        "http" -> "The feed returned HTTP ${status ?: "an error"}"
        "not_a_feed" -> "The URL doesn't point to a podcast feed"
        "invalid_url" -> "The feed URL is invalid"
        "port_not_allowed" -> "The feed's port is not allowed"
        "address_not_allowed" -> "The feed's address is not allowed"
        "host_not_found" -> "The feed's host can't be found"
        "too_many_redirects" -> "The feed redirects too many times"
        "invalid_redirect" -> "The feed redirects somewhere invalid"
        "too_large" -> "The feed is too large"
        "timeout" -> "The feed timed out"
        "connection_failed" -> "The feed's server can't be reached"
        else -> "The feed can't be fetched ($code)"
    }

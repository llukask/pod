package ltd.kilian.pod.net

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** What the OS reports about the default network: whether there is one, and whether it costs. */
data class NetworkState(val connected: Boolean, val metered: Boolean) {
    companion object {
        val OFFLINE = NetworkState(connected = false, metered = false)
    }
}

/**
 * Follows the default network through `ConnectivityManager` (spec 5.6, "Connectivity
 * regained"). Registered once for the process; the callback lives as long as it does.
 *
 * `connected` means the network claims internet access. It deliberately doesn't wait for
 * `VALIDATED`: a captive portal fails the request, which backs off like any network error, and
 * waiting for validation would hold back syncs on networks that block Google's probe.
 */
class NetworkMonitor(context: Context) {
    private val manager = context.getSystemService(ConnectivityManager::class.java)
    private val current = MutableStateFlow(read(manager.getNetworkCapabilities(manager.activeNetwork)))

    val state: StateFlow<NetworkState> = current.asStateFlow()

    init {
        manager.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
            override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
                current.value = read(capabilities)
            }

            override fun onLost(network: Network) {
                current.value = NetworkState.OFFLINE
            }
        })
    }

    private fun read(capabilities: NetworkCapabilities?): NetworkState =
        if (capabilities == null || !capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)) {
            NetworkState.OFFLINE
        } else {
            NetworkState(
                connected = true,
                metered = !capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED),
            )
        }
}

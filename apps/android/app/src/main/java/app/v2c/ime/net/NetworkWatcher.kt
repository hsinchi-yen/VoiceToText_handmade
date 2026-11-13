package app.v2c.ime.net

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities

/**
 * Calls [onChanged] when the default network appears or disappears (Wi-Fi <-> mobile, VPN on/off).
 * Callbacks arrive on a system thread; [onChanged] must hand work to a worker. Needs
 * ACCESS_NETWORK_STATE. Android glue, not unit tested.
 */
class NetworkWatcher(
    context: Context,
    private val onChanged: () -> Unit,
    /** The default network's VALIDATED capability flipped (cloud readiness); not a LAN change. */
    private val onValidatedChanged: () -> Unit = {},
) {
    private val manager = context.getSystemService(ConnectivityManager::class.java)
    private val tracker = NetworkChangeTracker()
    private var callback: ConnectivityManager.NetworkCallback? = null

    @Volatile private var lastValidated: Boolean? = null

    private val cb = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            if (tracker.onAvailable(network.networkHandle)) onChanged()
        }

        override fun onLost(network: Network) {
            if (tracker.onLost(network.networkHandle)) onChanged()
        }

        override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
            val validated = caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
            if (validated != lastValidated) {
                lastValidated = validated
                onValidatedChanged()
            }
        }
    }

    fun start() {
        if (callback != null || manager == null) return
        tracker.seedIfUnknown(manager.activeNetwork?.networkHandle)
        try {
            manager.registerDefaultNetworkCallback(cb)
            callback = cb
        } catch (_: RuntimeException) {
            // Too many callbacks registered or no permission; the keyboard-open refresh still works.
        }
    }

    fun stop() {
        val c = callback ?: return
        callback = null
        try {
            manager?.unregisterNetworkCallback(c)
        } catch (_: IllegalArgumentException) {
            // Already unregistered.
        }
    }
}

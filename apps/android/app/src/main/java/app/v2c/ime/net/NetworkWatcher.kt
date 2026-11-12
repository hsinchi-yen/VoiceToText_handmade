package app.v2c.ime.net

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network

/**
 * Calls [onChanged] when the default network appears or disappears (Wi-Fi <-> mobile, VPN on/off).
 * Callbacks arrive on a system thread; [onChanged] must hand work to a worker. Needs
 * ACCESS_NETWORK_STATE. Android glue, not unit tested.
 */
class NetworkWatcher(context: Context, private val onChanged: () -> Unit) {
    private val manager = context.getSystemService(ConnectivityManager::class.java)
    private val tracker = NetworkChangeTracker()
    private var callback: ConnectivityManager.NetworkCallback? = null

    private val cb = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            if (tracker.onAvailable(network.networkHandle)) onChanged()
        }

        override fun onLost(network: Network) {
            if (tracker.onLost(network.networkHandle)) onChanged()
        }
    }

    fun start() {
        if (callback != null || manager == null) return
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

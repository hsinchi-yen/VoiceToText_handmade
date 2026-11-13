package app.v2c.ime.net

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import app.v2c.ime.cloud.NetworkValidity

/**
 * [NetworkValidity] from the default network's `NET_CAPABILITY_VALIDATED` (the system has
 * confirmed internet access); the cloud is never probed. Needs ACCESS_NETWORK_STATE.
 * Android glue, not unit tested.
 */
class ConnectivityNetworkValidity(context: Context) : NetworkValidity {
    private val manager = context.getSystemService(ConnectivityManager::class.java)

    override fun isValidated(): Boolean {
        val m = manager ?: return false
        val caps = m.getNetworkCapabilities(m.activeNetwork ?: return false) ?: return false
        return caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
    }
}

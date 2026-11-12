package app.v2c.ime.net

/**
 * Decides whether a default-network callback is a real change. Registering a default-network
 * callback fires onAvailable at once for the current network (every time the keyboard opens);
 * that must not drop the LAN cache. Pure Kotlin, unit tested.
 */
class NetworkChangeTracker {
    private var current: Long? = null

    /** True when [id] differs from the network seen last (Wi-Fi <-> mobile, VPN switch). */
    @Synchronized
    fun onAvailable(id: Long): Boolean {
        if (current == id) return false
        current = id
        return true
    }

    /** True when the network that was current is lost; a late loss of an older one is ignored. */
    @Synchronized
    fun onLost(id: Long): Boolean {
        if (current != id) return false
        current = null
        return true
    }
}

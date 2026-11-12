package app.v2c.ime.net

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class NetworkChangeTrackerTest {
    @Test
    fun firstNetworkCountsAsChangeButReRegistrationOnTheSameOneDoesNot() {
        val t = NetworkChangeTracker()
        assertTrue(t.onAvailable(1))
        assertFalse(t.onAvailable(1)) // keyboard reopened: immediate callback, same network
    }

    @Test
    fun aDifferentNetworkIsAChange() {
        val t = NetworkChangeTracker()
        t.onAvailable(1)
        assertTrue(t.onAvailable(2))
    }

    @Test
    fun lossOfTheCurrentNetworkIsAChangeButLateLossOfAnOldOneIsNot() {
        val t = NetworkChangeTracker()
        t.onAvailable(1)
        t.onAvailable(2)
        assertFalse(t.onLost(1)) // old Wi-Fi reported lost after the switch
        assertTrue(t.onLost(2))
        assertTrue(t.onAvailable(2)) // coming back is a change again
    }

    @Test
    fun seedingTheCurrentNetworkMakesTheFirstCallbackNoChange() {
        val t = NetworkChangeTracker()
        t.seedIfUnknown(7L)
        assertFalse(t.onAvailable(7L))
        assertTrue(t.onAvailable(8L))
    }

    @Test
    fun seedingNeverHidesAChangeMadeWhileHidden() {
        val t = NetworkChangeTracker()
        t.onAvailable(7L)
        t.seedIfUnknown(8L) // keyboard reopened on another network
        assertTrue(t.onAvailable(8L))
        t.seedIfUnknown(null)
        assertFalse(t.onAvailable(8L))
    }
}

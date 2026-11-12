package app.v2c.ime.net

import app.v2c.ime.FakeFfi
import app.v2c.ime.MapProber
import app.v2c.ime.RD_TEA_URL
import app.v2c.ime.TN_TEA_URL
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.loadedCatalog
import app.v2c.ime.newDetector
import app.v2c.ime.testExecutor
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class LanDetectorTest {

    @Test
    fun timeoutConstantIs800Ms() {
        assertEquals(800, LanDetector.PROBE_TIMEOUT_MS)
    }

    @Test
    fun beforeAnyProbeTheLanIsUnreachableAndCurrentNeverProbes() {
        val prober = MapProber()
        val d = newDetector(prober)
        assertEquals(LanSite.UNREACHABLE, d.current())
        assertTrue(prober.requested.isEmpty())
    }

    @Test
    fun probesModelsEndpointOfBothTeaServices() {
        val prober = MapProber() // both answer "down" at once, so both are waited for
        newDetector(prober).refresh()
        assertTrue(prober.requested.contains(TN_TEA_URL))
        // The RD base url in the fake ends with a slash: no double slash in the probe url.
        assertTrue(prober.requested.contains(RD_TEA_URL))
    }

    @Test
    fun tnWinsWhenItAnswersFirst() {
        val prober = MapProber()
        prober.up(TN_TEA_URL, afterMs = 10)
        prober.up(RD_TEA_URL, afterMs = 400)
        assertEquals(LanSite.TN, newDetector(prober).refresh())
    }

    @Test
    fun rdWinsWhenItAnswersFirst() {
        val prober = MapProber()
        prober.up(TN_TEA_URL, afterMs = 400)
        prober.up(RD_TEA_URL, afterMs = 10)
        val started = System.nanoTime()
        assertEquals(LanSite.RD, newDetector(prober).refresh())
        // First responder wins: it must not wait for the slower endpoint.
        assertTrue((System.nanoTime() - started) / 1_000_000 < 350)
    }

    @Test
    fun aFailedFastEndpointDoesNotBlockTheOtherOne() {
        val prober = MapProber()
        prober.up(RD_TEA_URL, afterMs = 100) // TN fails immediately
        assertEquals(LanSite.RD, newDetector(prober).refresh())
    }

    @Test
    fun bothDownMeansUnreachable() {
        assertEquals(LanSite.UNREACHABLE, newDetector(MapProber()).refresh())
    }

    @Test
    fun hangingServerDoesNotStallBeyondTheTimeout() {
        val prober = MapProber()
        prober.hang(TN_TEA_URL)
        prober.hang(RD_TEA_URL)
        val d = newDetector(prober, timeoutMs = 200)
        val started = System.nanoTime()
        assertEquals(LanSite.UNREACHABLE, d.refresh())
        val ms = (System.nanoTime() - started) / 1_000_000
        assertTrue("took $ms ms", ms in 150..1500)
    }

    @Test
    fun hangingTnStillLetsRdWin() {
        val prober = MapProber()
        prober.hang(TN_TEA_URL)
        prober.up(RD_TEA_URL, afterMs = 20)
        assertEquals(LanSite.RD, newDetector(prober, timeoutMs = 500).refresh())
    }

    @Test
    fun aProberThatThrowsCountsAsDown() {
        val throwing = HttpProber { _, _ -> throw IllegalStateException("boom") }
        val d = LanDetector(loadedCatalog(), throwing, testExecutor, timeoutMs = 200)
        assertEquals(LanSite.UNREACHABLE, d.refresh())
    }

    @Test
    fun resultIsCachedUntilTheNextRefresh() {
        val prober = MapProber()
        prober.up(TN_TEA_URL)
        val d = newDetector(prober)
        d.refresh()
        val probes = prober.requested.size
        repeat(5) { assertEquals(LanSite.TN, d.current()) }
        assertEquals(probes, prober.requested.size)
    }

    @Test
    fun networkChangeInvalidatesThenReprobeFindsTheNewLan() {
        val prober = MapProber()
        prober.up(TN_TEA_URL)
        val d = newDetector(prober)
        assertEquals(LanSite.TN, d.refresh())

        // Wi-Fi switched: stale answer is dropped at once...
        d.invalidate()
        assertEquals(LanSite.UNREACHABLE, d.current())

        // ...and a re-probe on the new network sees only RD.
        prober.down(TN_TEA_URL)
        prober.up(RD_TEA_URL)
        assertEquals(LanSite.RD, d.refresh())
        assertEquals(LanSite.RD, d.current())
    }

    @Test
    fun leavingTheCompanyNetworkTurnsTheCacheUnreachable() {
        val prober = MapProber()
        prober.up(TN_TEA_URL)
        val d = newDetector(prober)
        d.refresh()
        d.invalidate()
        prober.down(TN_TEA_URL)
        assertEquals(LanSite.UNREACHABLE, d.refresh())
        assertEquals(LanSite.UNREACHABLE, d.current())
    }

    @Test
    fun anOlderRefreshCannotOverwriteANewerInvalidate() {
        val prober = MapProber()
        prober.up(TN_TEA_URL, afterMs = 300)
        val d = newDetector(prober)
        val t = Thread { d.refresh() }
        t.start()
        Thread.sleep(80)
        d.invalidate() // network changed while the first probe was in flight
        t.join()
        assertEquals(LanSite.UNREACHABLE, d.current())
    }

    @Test
    fun withoutLoadedPresetsNothingIsProbed() {
        val prober = MapProber()
        val d = LanDetector(LanCatalog(FakeFfi()), prober, testExecutor, timeoutMs = 100)
        assertEquals(LanSite.UNREACHABLE, d.refresh())
        assertTrue(prober.requested.isEmpty())
    }

    @Test
    fun resultCallbackGetsSiteAndLatency() {
        val prober = MapProber()
        prober.up(TN_TEA_URL)
        var seen: Pair<LanSite, Long>? = null
        val d = LanDetector(loadedCatalog(), prober, testExecutor) { s, ms -> seen = s to ms }
        d.refresh()
        assertEquals(LanSite.TN, seen!!.first)
        assertTrue(seen!!.second >= 0)
    }

    @Test
    fun catalogPicksQwenPerSite() {
        val c = loadedCatalog()
        assertEquals("tn-qwen", c.llmServiceId(LanSite.TN))
        assertEquals("rd-qwen", c.llmServiceId(LanSite.RD))
        assertEquals(null, c.llmServiceId(LanSite.UNREACHABLE))
    }
}

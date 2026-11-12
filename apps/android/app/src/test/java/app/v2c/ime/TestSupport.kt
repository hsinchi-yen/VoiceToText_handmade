package app.v2c.ime

import app.v2c.ime.asr.EventLog
import app.v2c.ime.asr.local.LocalAsr
import app.v2c.ime.ffi.AsrHint
import app.v2c.ime.ffi.AsrPick
import app.v2c.ime.ffi.AsrPlan
import app.v2c.ime.ffi.AsrSource
import app.v2c.ime.ffi.FfiErrorCode
import app.v2c.ime.ffi.LanServiceInfo
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.ffi.V2cFfi
import app.v2c.ime.ffi.V2cFfiException
import app.v2c.ime.net.HttpProber
import app.v2c.ime.net.LanCatalog
import app.v2c.ime.net.LanDetector
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors

const val TN_TEA_URL = "http://10.20.30.23:8001/v1/models"
const val RD_TEA_URL = "http://10.88.95.2:8001/v1/models"

/** Test double for the uniffi facade. [chooseAsr] mirrors the routing table of plan section 8.1. */
class FakeFfi : V2cFfi {
    val transcribeCalls = CopyOnWriteArrayList<Pair<Int, String>>() // (byte count, service id)
    val translateCalls = CopyOnWriteArrayList<Pair<String, String>>() // (text, service id)
    var transcribe: (ByteArray, String) -> String = { _, _ -> "raw" }
    var translateImpl: (String, String) -> String = { t, _ -> "en:$t" }
    var zhTwFails = false

    override fun loadLanServices(tnToml: String, rdToml: String) = listOf(
        LanServiceInfo("tn-tea-asr", "http://10.20.30.23:8001/v1", true, false, 3000),
        LanServiceInfo("tn-breeze-asr", "http://10.20.30.23:8002/v1", true, false, 8000),
        LanServiceInfo("tn-qwen", "http://10.20.30.23:8000/v1", false, true, 3000),
        LanServiceInfo("rd-tea-asr", "http://10.88.95.2:8001/v1/", true, false, 3000),
        LanServiceInfo("rd-breeze-asr", "http://10.88.95.2:8002/v1", true, false, 8000),
        LanServiceInfo("rd-qwen", "http://10.88.95.2:8000/v1", false, true, 3000),
    )

    override fun chooseAsr(lan: LanSite, systemAvailable: Boolean, localModelReady: Boolean, pick: AsrPick): AsrPlan {
        val lanUp = lan != LanSite.UNREACHABLE
        fun none(h: AsrHint) = AsrPlan(AsrSource.NONE, AsrSource.NONE, false, h)
        return when (pick) {
            AsrPick.AUTO -> when {
                lanUp -> AsrPlan(
                    AsrSource.LAN_TEA,
                    if (localModelReady) AsrSource.LOCAL else AsrSource.NONE,
                    !localModelReady,
                    AsrHint.NONE,
                )
                systemAvailable -> AsrPlan(
                    AsrSource.SYSTEM,
                    if (localModelReady) AsrSource.LOCAL else AsrSource.NONE,
                    true,
                    AsrHint.NONE,
                )
                localModelReady -> AsrPlan(AsrSource.LOCAL, AsrSource.NONE, true, AsrHint.NONE)
                else -> none(AsrHint.NO_SOURCE_AVAILABLE)
            }
            AsrPick.TEA -> if (lanUp) AsrPlan(AsrSource.LAN_TEA, AsrSource.NONE, true, AsrHint.NONE) else none(AsrHint.LAN_UNREACHABLE)
            AsrPick.BREEZE -> if (lanUp) AsrPlan(AsrSource.LAN_BREEZE, AsrSource.NONE, true, AsrHint.NONE) else none(AsrHint.LAN_UNREACHABLE)
            AsrPick.SYSTEM -> if (systemAvailable) AsrPlan(AsrSource.SYSTEM, AsrSource.NONE, true, AsrHint.NONE) else none(AsrHint.SYSTEM_UNAVAILABLE)
            AsrPick.LOCAL -> if (localModelReady) AsrPlan(AsrSource.LOCAL, AsrSource.NONE, true, AsrHint.NONE) else none(AsrHint.LOCAL_MODEL_NOT_READY)
        }
    }

    override fun lanServiceId(lan: LanSite, source: AsrSource): String? {
        if (lan == LanSite.UNREACHABLE) return null
        val p = lan.servicePrefix
        return when (source) {
            AsrSource.LAN_TEA -> p + "tea-asr"
            AsrSource.LAN_BREEZE -> p + "breeze-asr"
            else -> null
        }
    }

    override fun transcribePcm(pcmLe: ByteArray, rate: Int, serviceId: String): String {
        transcribeCalls += pcmLe.size to serviceId
        return transcribe(pcmLe, serviceId)
    }

    override fun translate(text: String, serviceId: String): String {
        translateCalls += text to serviceId
        return translateImpl(text, serviceId)
    }

    override fun toZhTw(text: String): String {
        if (zhTwFails) throw V2cFfiException(FfiErrorCode.INTERNAL)
        return if (text.isBlank()) text else "tw:$text"
    }
}

class FakeLocalAsr(var ready: Boolean = true) : LocalAsr {
    val calls = CopyOnWriteArrayList<ShortArray>()
    var result: (ShortArray) -> String = { "local" }
    override fun isReady() = ready
    override fun transcribe(pcm: ShortArray, rate: Int): String {
        calls += pcm
        return result(pcm)
    }
}

class ListLog : EventLog {
    val lines = CopyOnWriteArrayList<String>()
    override fun log(line: String) {
        lines += line
    }
}

/** A prober that answers per URL; unknown URLs fail. */
class MapProber(
    private val upAfterMs: MutableMap<String, Long> = mutableMapOf(),
    private val hangUrls: MutableSet<String> = mutableSetOf(),
) : HttpProber {
    val requested = CopyOnWriteArrayList<String>()

    fun up(url: String, afterMs: Long = 0) {
        upAfterMs[url] = afterMs
    }

    fun down(url: String) {
        upAfterMs.remove(url)
    }

    fun hang(url: String) {
        hangUrls += url
    }

    override fun get(url: String, timeoutMs: Int): Boolean {
        requested += url
        if (url in hangUrls) {
            Thread.sleep(10_000) // interrupted by the detector when it gives up
        }
        val delay = upAfterMs[url] ?: return false
        if (delay > 0) Thread.sleep(delay)
        return true
    }
}

val testExecutor = Executors.newCachedThreadPool { r -> Thread(r).apply { isDaemon = true } }

fun loadedCatalog(ffi: V2cFfi = FakeFfi()): LanCatalog = LanCatalog(ffi).also { it.load("tn", "rd") }

fun newDetector(prober: HttpProber, ffi: V2cFfi = FakeFfi(), timeoutMs: Int = 800): LanDetector =
    LanDetector(loadedCatalog(ffi), prober, testExecutor, timeoutMs = timeoutMs)

/** A detector whose cache already holds [site]. */
fun detectorAt(site: LanSite, ffi: V2cFfi = FakeFfi()): LanDetector {
    val prober = MapProber()
    when (site) {
        LanSite.TN -> prober.up(TN_TEA_URL)
        LanSite.RD -> prober.up(RD_TEA_URL)
        LanSite.UNREACHABLE -> Unit
    }
    return newDetector(prober, ffi).also { it.refresh() }
}

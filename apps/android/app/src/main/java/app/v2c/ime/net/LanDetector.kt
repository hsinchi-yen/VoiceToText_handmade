package app.v2c.ime.net

import app.v2c.ime.ffi.AsrSource
import app.v2c.ime.ffi.LanServiceInfo
import app.v2c.ime.ffi.LanSite
import app.v2c.ime.ffi.V2cFfi
import java.util.concurrent.Callable
import java.util.concurrent.ExecutorCompletionService
import java.util.concurrent.Executor
import java.util.concurrent.Future
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong

/** Blocking HTTP check. Android implementation: [HttpUrlConnectionProber]; tests use fakes. */
fun interface HttpProber {
    /** True when a GET of [url] got an HTTP answer within [timeoutMs]. Must not throw. */
    fun get(url: String, timeoutMs: Int): Boolean
}

fun interface Clock {
    fun nowMs(): Long

    object System : Clock {
        override fun nowMs(): Long = java.lang.System.nanoTime() / 1_000_000L
    }
}

/**
 * What the preset files say about the two LANs: the TEA-ASR probe URL and the Qwen service id.
 * Empty until [load] succeeded (then the LAN counts as unreachable).
 */
class LanCatalog(private val ffi: V2cFfi) {
    @Volatile private var services: List<LanServiceInfo> = emptyList()

    val isLoaded: Boolean get() = services.isNotEmpty()

    /** Throws [app.v2c.ime.ffi.V2cFfiException] when the presets are bad. Call off the main thread. */
    fun load(tnToml: String, rdToml: String) {
        services = ffi.loadLanServices(tnToml, rdToml)
    }

    /** `GET <base>/models` of the site's TEA-ASR service, or null when unknown. */
    fun probeUrl(site: LanSite): String? {
        val id = ffi.lanServiceId(site, AsrSource.LAN_TEA) ?: return null
        val base = services.firstOrNull { it.id == id }?.baseUrl ?: return null
        return base.trimEnd('/') + "/models"
    }

    /** Id of the site's LLM service (Qwen), used by translate. */
    fun llmServiceId(site: LanSite): String? =
        services.firstOrNull { it.hasLlm && site != LanSite.UNREACHABLE && it.id.startsWith(site.servicePrefix) }?.id
}

/**
 * Detects which company LAN (TN or RD) is reachable and caches the answer (plan section 8.1).
 *
 * - [current] never blocks and never probes: it returns the cached site.
 * - [refresh] probes both TEA endpoints in parallel; the first one that answers wins; gives up
 *   after [timeoutMs] (all endpoints failing or hanging -> UNREACHABLE).
 * - [invalidate] (network changed) drops the cache to UNREACHABLE immediately so the next
 *   utterance does not wait on a stale LAN; a [refresh] then restores it.
 *
 * Results of an older refresh never overwrite a newer invalidate/refresh.
 */
class LanDetector(
    private val catalog: LanCatalog,
    private val prober: HttpProber,
    private val executor: Executor,
    private val clock: Clock = Clock.System,
    private val timeoutMs: Int = PROBE_TIMEOUT_MS,
    private val onResult: (site: LanSite, latencyMs: Long) -> Unit = { _, _ -> },
) {
    @Volatile private var cached: LanSite = LanSite.UNREACHABLE
    private val generation = AtomicLong(0)

    fun current(): LanSite = cached

    fun invalidate() {
        generation.incrementAndGet()
        cached = LanSite.UNREACHABLE
    }

    /** Blocking, at most about [timeoutMs]. Returns (and caches) the detected site. */
    fun refresh(): LanSite {
        val gen = generation.incrementAndGet()
        val started = clock.nowMs()
        val site = probe()
        if (generation.get() == gen) cached = site
        onResult(site, clock.nowMs() - started)
        return site
    }

    private fun probe(): LanSite {
        val sites = listOf(LanSite.TN, LanSite.RD)
        val completion = ExecutorCompletionService<LanSite>(executor)
        val futures = ArrayList<Future<LanSite>>()
        for (site in sites) {
            val url = catalog.probeUrl(site) ?: continue
            futures += completion.submit(
                Callable { if (prober.get(url, timeoutMs)) site else LanSite.UNREACHABLE },
            )
        }
        var winner = LanSite.UNREACHABLE
        val deadline = clock.nowMs() + timeoutMs
        try {
            var pending = futures.size
            while (pending > 0 && winner == LanSite.UNREACHABLE) {
                // Real time is used for the wait itself; the clock only defines the budget.
                val remaining = (deadline - clock.nowMs()).coerceAtLeast(0)
                val done = completion.poll(remaining, TimeUnit.MILLISECONDS) ?: break
                pending--
                winner = try {
                    done.get()
                } catch (_: Exception) {
                    LanSite.UNREACHABLE
                }
            }
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
        } finally {
            futures.forEach { it.cancel(true) }
        }
        return winner
    }

    companion object {
        /** Per plan section 8.1. */
        const val PROBE_TIMEOUT_MS = 800
    }
}

package app.v2c.ime.cloud

import app.v2c.ime.asr.EventLog
import app.v2c.ime.ffi.CloudUse
import app.v2c.ime.ffi.V2cFfi
import app.v2c.ime.ffi.V2cFfiException

/** True when the default network has `NET_CAPABILITY_VALIDATED` (plan section 8.3). */
fun interface NetworkValidity {
    fun isValidated(): Boolean
}

/**
 * What the recognition router, the translator and the keyboard UI ask about the cloud services.
 * [None] is used when nothing is configured, which keeps today's behaviour unchanged.
 */
interface CloudRouting {
    /** A recognition service is set up (drives the "cloud" entry in the source menu). */
    val asrConfigured: Boolean

    /** Recognition is configured, its key is readable and the network is validated. */
    fun asrReady(): Boolean

    /** FFI service id for recognition, or null when none is configured. */
    fun asrServiceId(): String?

    /** Chip text, e.g. "Gemini"; null when none is configured. */
    fun asrLabel(): String?

    /** Translation is configured, its key is readable and the network is validated. */
    fun translateReady(): Boolean

    fun translateServiceId(): String?

    object None : CloudRouting {
        override val asrConfigured = false
        override fun asrReady() = false
        override fun asrServiceId(): String? = null
        override fun asrLabel(): String? = null
        override fun translateReady() = false
        override fun translateServiceId(): String? = null
    }
}

/**
 * Production [CloudRouting]: reads [CloudRepository], hands the services and keys to the FFI in
 * [reload], and answers readiness from the loaded snapshot plus the live network state.
 * [reload] blocks (Keystore, FFI); call it on Dispatchers.IO. Logs provider names only.
 */
class CloudRuntime(
    private val repository: CloudRepository,
    private val ffi: V2cFfi,
    private val network: NetworkValidity,
    private val log: EventLog = EventLog { },
) : CloudRouting {

    @Volatile private var snapshot: CloudSnapshot = CloudSnapshot.EMPTY

    /** The last [reload] reached the FFI (or there was nothing to send). */
    @Volatile private var loaded = false

    @Volatile private var everPushed = false

    /**
     * Re-reads settings and keys and calls `set_cloud_services`. Returns [loaded]. [force] pushes
     * even an empty set that was never pushed (clears a tested-but-unsaved draft in Rust memory).
     * Never throws: a failing Keystore or storage means "not ready".
     */
    fun reload(force: Boolean = false): Boolean {
        val snap = try {
            repository.snapshot()
        } catch (e: Exception) {
            log.log("cloud load failed type=${e.javaClass.simpleName}")
            snapshot = CloudSnapshot.EMPTY
            loaded = false
            return false
        }
        snapshot = snap
        if (snap.services.isEmpty() && !everPushed && !force) {
            loaded = true
            return true
        }
        loaded = try {
            ffi.setCloudServices(snap.configs(), snap.keysForFfi())
            everPushed = true
            true
        } catch (e: V2cFfiException) {
            log.log("cloud load failed code=${e.code}")
            false
        }
        log.log(
            "cloud load services=${snap.services.size} asr=${snap.serviceFor(CloudUse.ASR)?.kind?.label} " +
                "translate=${snap.serviceFor(CloudUse.TRANSLATE)?.kind?.label} loaded=$loaded",
        )
        return loaded
    }

    override val asrConfigured: Boolean get() = snapshot.serviceFor(CloudUse.ASR) != null

    override fun asrReady(): Boolean = ready(CloudUse.ASR)

    override fun asrServiceId(): String? = snapshot.serviceFor(CloudUse.ASR)?.id

    override fun asrLabel(): String? = snapshot.serviceFor(CloudUse.ASR)?.kind?.label

    override fun translateReady(): Boolean = ready(CloudUse.TRANSLATE)

    override fun translateServiceId(): String? = snapshot.serviceFor(CloudUse.TRANSLATE)?.id

    private fun ready(use: CloudUse): Boolean =
        loaded && snapshot.readyServiceFor(use) != null && network.isValidated()
}

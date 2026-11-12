package app.v2c.ime.ffi

import uniffi.v2c_ffi.AsrHint as FAsrHint
import uniffi.v2c_ffi.AsrPick as FAsrPick
import uniffi.v2c_ffi.AsrSource as FAsrSource
import uniffi.v2c_ffi.FfiException
import uniffi.v2c_ffi.Lan as FLan

/**
 * Production [V2cFfi]: maps between the generated uniffi types and the mirror types. Not unit
 * tested (needs the native library); the mapping is deliberately mechanical.
 */
class UniffiV2cFfi : V2cFfi {

    override fun loadLanServices(tnToml: String, rdToml: String): List<LanServiceInfo> = guard {
        uniffi.v2c_ffi.loadLanServices(tnToml, rdToml).services.map {
            LanServiceInfo(
                id = it.id,
                baseUrl = it.baseUrl,
                hasAsr = it.hasAsr,
                hasLlm = it.hasLlm,
                asrTimeoutMs = it.asrTimeoutMs.toLong(),
            )
        }
    }

    override fun chooseAsr(
        lan: LanSite,
        systemAvailable: Boolean,
        localModelReady: Boolean,
        pick: AsrPick,
    ): AsrPlan = guard {
        val p = uniffi.v2c_ffi.chooseAsr(lan.toFfi(), systemAvailable, localModelReady, pick.toFfi())
        AsrPlan(p.primary.toApp(), p.fallback.toApp(), p.retrySpeak, p.hint.toApp())
    }

    override fun lanServiceId(lan: LanSite, source: AsrSource): String? = guard {
        uniffi.v2c_ffi.lanServiceId(lan.toFfi(), source.toFfi())
    }

    override fun transcribePcm(pcmLe: ByteArray, rate: Int, serviceId: String): String = guard {
        uniffi.v2c_ffi.transcribePcm(pcmLe, rate.toUInt(), serviceId)
    }

    override fun translate(text: String, serviceId: String): String = guard {
        uniffi.v2c_ffi.translate(text, serviceId)
    }

    override fun toZhTw(text: String): String = guard { uniffi.v2c_ffi.toZhTw(text) }

    private inline fun <T> guard(block: () -> T): T = try {
        block()
    } catch (e: FfiException) {
        throw V2cFfiException(e.toCode())
    } catch (e: LinkageError) {
        // Native library missing for this ABI (UnsatisfiedLinkError, ExceptionInInitializerError,
        // NoClassDefFoundError from the JNA/uniffi static init): degrade instead of crashing the IME.
        throw V2cFfiException(FfiErrorCode.INTERNAL)
    } catch (e: uniffi.v2c_ffi.InternalException) {
        throw V2cFfiException(FfiErrorCode.INTERNAL)
    }

    private fun FfiException.toCode(): FfiErrorCode = when (this) {
        is FfiException.Config -> FfiErrorCode.CONFIG
        is FfiException.NotLoaded -> FfiErrorCode.NOT_LOADED
        is FfiException.UnknownService -> FfiErrorCode.UNKNOWN_SERVICE
        is FfiException.Unsupported -> FfiErrorCode.UNSUPPORTED
        is FfiException.Unauthorized -> FfiErrorCode.UNAUTHORIZED
        is FfiException.RateLimited -> FfiErrorCode.RATE_LIMITED
        is FfiException.Unreachable -> FfiErrorCode.UNREACHABLE
        is FfiException.BadResponse -> FfiErrorCode.BAD_RESPONSE
        is FfiException.Server -> FfiErrorCode.SERVER
        is FfiException.Timeout -> FfiErrorCode.TIMEOUT
        is FfiException.InvalidAudio -> FfiErrorCode.INVALID_AUDIO
        is FfiException.EmptyText -> FfiErrorCode.EMPTY_TEXT
        is FfiException.Internal -> FfiErrorCode.INTERNAL
    }

    private fun LanSite.toFfi(): FLan = when (this) {
        LanSite.TN -> FLan.TN
        LanSite.RD -> FLan.RD
        LanSite.UNREACHABLE -> FLan.UNREACHABLE
    }

    private fun AsrPick.toFfi(): FAsrPick = when (this) {
        AsrPick.AUTO -> FAsrPick.AUTO
        AsrPick.TEA -> FAsrPick.TEA
        AsrPick.BREEZE -> FAsrPick.BREEZE
        AsrPick.SYSTEM -> FAsrPick.SYSTEM
        AsrPick.LOCAL -> FAsrPick.LOCAL
    }

    private fun AsrSource.toFfi(): FAsrSource = when (this) {
        AsrSource.NONE -> FAsrSource.NONE
        AsrSource.LAN_TEA -> FAsrSource.LAN_TEA
        AsrSource.LAN_BREEZE -> FAsrSource.LAN_BREEZE
        AsrSource.SYSTEM -> FAsrSource.SYSTEM
        AsrSource.LOCAL -> FAsrSource.LOCAL
    }

    private fun FAsrSource.toApp(): AsrSource = when (this) {
        FAsrSource.NONE -> AsrSource.NONE
        FAsrSource.LAN_TEA -> AsrSource.LAN_TEA
        FAsrSource.LAN_BREEZE -> AsrSource.LAN_BREEZE
        FAsrSource.SYSTEM -> AsrSource.SYSTEM
        FAsrSource.LOCAL -> AsrSource.LOCAL
    }

    private fun FAsrHint.toApp(): AsrHint = when (this) {
        FAsrHint.NONE -> AsrHint.NONE
        FAsrHint.NO_SOURCE_AVAILABLE -> AsrHint.NO_SOURCE_AVAILABLE
        FAsrHint.LAN_UNREACHABLE -> AsrHint.LAN_UNREACHABLE
        FAsrHint.SYSTEM_UNAVAILABLE -> AsrHint.SYSTEM_UNAVAILABLE
        FAsrHint.LOCAL_MODEL_NOT_READY -> AsrHint.LOCAL_MODEL_NOT_READY
    }
}

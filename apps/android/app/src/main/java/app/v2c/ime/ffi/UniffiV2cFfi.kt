package app.v2c.ime.ffi

import uniffi.v2c_ffi.AsrHint as FAsrHint
import uniffi.v2c_ffi.AsrPick as FAsrPick
import uniffi.v2c_ffi.AsrSource as FAsrSource
import uniffi.v2c_ffi.CloudProvider as FCloudProvider
import uniffi.v2c_ffi.CloudServiceConfig as FCloudServiceConfig
import uniffi.v2c_ffi.CloudUse as FCloudUse
import uniffi.v2c_ffi.FfiException
import uniffi.v2c_ffi.ImportStatus as FImportStatus
import uniffi.v2c_ffi.SkipReason as FSkipReason
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
        cloudReady: Boolean,
        systemAvailable: Boolean,
        localModelReady: Boolean,
        pick: AsrPick,
    ): AsrPlan = guard {
        val p = uniffi.v2c_ffi.chooseAsr(lan.toFfi(), cloudReady, systemAvailable, localModelReady, pick.toFfi())
        AsrPlan(p.primary.toApp(), p.fallback.toApp(), p.retrySpeak, p.hint.toApp(), p.fallback2.toApp())
    }

    override fun setCloudServices(services: List<CloudServiceConfig>, keys: Map<String, String>) = guard {
        uniffi.v2c_ffi.setCloudServices(
            services.map { it.toFfi() },
            keys,
        )
    }

    override fun testConnection(serviceId: String, use: CloudUse): CloudTestResult = guard {
        val r = uniffi.v2c_ffi.testConnection(serviceId, use.toFfi())
        CloudTestResult(r.latencyMs.toLong(), r.error?.toCode())
    }

    override fun parseConfigFile(
        text: String,
        current: List<CloudServiceConfig>,
        storedKeys: List<String>,
    ): ConfigFilePreview = guard {
        val p = uniffi.v2c_ffi.parseConfigFile(text, current.map { it.toFfi() }, storedKeys)
        ConfigFilePreview(
            errorCode = p.errorCode,
            errorLine = p.errorLine?.toInt(),
            title = p.title,
            exportedFrom = p.exportedFrom,
            services = p.services.map {
                ImportedService(
                    sourceId = it.sourceId,
                    config = it.config.toApp(),
                    status = when (it.status) {
                        FImportStatus.ADDED -> ImportStatus.ADDED
                        FImportStatus.UPDATED -> ImportStatus.UPDATED
                        FImportStatus.UNCHANGED -> ImportStatus.UNCHANGED
                    },
                    needsKey = it.needsKey,
                    dropOldKey = it.dropOldKey,
                )
            },
            skipped = p.skipped.map {
                SkippedService(
                    it.id,
                    when (it.reason) {
                        FSkipReason.BUILTIN -> SkipReason.BUILTIN
                        FSkipReason.HTTP_ENDPOINT -> SkipReason.HTTP_ENDPOINT
                        FSkipReason.OFFICIAL_BASE_URL -> SkipReason.OFFICIAL_BASE_URL
                        FSkipReason.DUPLICATE_PROVIDER -> SkipReason.DUPLICATE_PROVIDER
                    },
                )
            },
            asrService = p.asrService,
            translateService = p.translateService,
            ignored = p.ignored,
        )
    }

    override fun exportConfigFile(
        services: List<CloudServiceConfig>,
        asrId: String?,
        translateId: String?,
        storedKeys: List<String>,
    ): ConfigFileExport = guard {
        val e = uniffi.v2c_ffi.exportConfigFile(services.map { it.toFfi() }, asrId, translateId, storedKeys)
        ConfigFileExport(e.text, e.skipped)
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
        AsrPick.CLOUD -> FAsrPick.CLOUD
        AsrPick.SYSTEM -> FAsrPick.SYSTEM
        AsrPick.LOCAL -> FAsrPick.LOCAL
    }

    private fun AsrSource.toFfi(): FAsrSource = when (this) {
        AsrSource.NONE -> FAsrSource.NONE
        AsrSource.LAN_TEA -> FAsrSource.LAN_TEA
        AsrSource.LAN_BREEZE -> FAsrSource.LAN_BREEZE
        AsrSource.CLOUD -> FAsrSource.CLOUD
        AsrSource.SYSTEM -> FAsrSource.SYSTEM
        AsrSource.LOCAL -> FAsrSource.LOCAL
    }

    private fun FAsrSource.toApp(): AsrSource = when (this) {
        FAsrSource.NONE -> AsrSource.NONE
        FAsrSource.LAN_TEA -> AsrSource.LAN_TEA
        FAsrSource.LAN_BREEZE -> AsrSource.LAN_BREEZE
        FAsrSource.SYSTEM -> AsrSource.SYSTEM
        FAsrSource.LOCAL -> AsrSource.LOCAL
        FAsrSource.CLOUD -> AsrSource.CLOUD
    }

    private fun FAsrHint.toApp(): AsrHint = when (this) {
        FAsrHint.NONE -> AsrHint.NONE
        FAsrHint.NO_SOURCE_AVAILABLE -> AsrHint.NO_SOURCE_AVAILABLE
        FAsrHint.LAN_UNREACHABLE -> AsrHint.LAN_UNREACHABLE
        FAsrHint.SYSTEM_UNAVAILABLE -> AsrHint.SYSTEM_UNAVAILABLE
        FAsrHint.LOCAL_MODEL_NOT_READY -> AsrHint.LOCAL_MODEL_NOT_READY
        FAsrHint.CLOUD_NOT_READY -> AsrHint.CLOUD_NOT_READY
    }

    private fun CloudServiceConfig.toFfi() = FCloudServiceConfig(
        id = id,
        provider = provider.toFfi(),
        model = model,
        asrModel = asrModel,
        baseUrl = baseUrl,
        uses = uses.map { it.toFfi() },
        asrTimeoutMs = asrTimeoutMs?.toULong(),
    )

    private fun FCloudServiceConfig.toApp() = CloudServiceConfig(
        id = id,
        provider = when (provider) {
            FCloudProvider.GEMINI -> CloudProvider.GEMINI
            FCloudProvider.OPENAI -> CloudProvider.OPENAI
            FCloudProvider.CLAUDE -> CloudProvider.CLAUDE
            FCloudProvider.CUSTOM -> CloudProvider.CUSTOM
        },
        model = model,
        asrModel = asrModel,
        baseUrl = baseUrl,
        uses = uses.map { if (it == FCloudUse.ASR) CloudUse.ASR else CloudUse.TRANSLATE },
        asrTimeoutMs = asrTimeoutMs?.toLong(),
    )

    private fun CloudProvider.toFfi(): FCloudProvider = when (this) {
        CloudProvider.GEMINI -> FCloudProvider.GEMINI
        CloudProvider.OPENAI -> FCloudProvider.OPENAI
        CloudProvider.CLAUDE -> FCloudProvider.CLAUDE
        CloudProvider.CUSTOM -> FCloudProvider.CUSTOM
    }

    private fun CloudUse.toFfi(): FCloudUse = when (this) {
        CloudUse.ASR -> FCloudUse.ASR
        CloudUse.TRANSLATE -> FCloudUse.TRANSLATE
    }
}

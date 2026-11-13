package app.v2c.ime.cloud

import app.v2c.ime.ffi.CloudProvider
import app.v2c.ime.ffi.CloudServiceConfig
import app.v2c.ime.ffi.CloudUse

/**
 * The cloud providers the setup page offers (plan section 8.3). One service per provider, so the
 * provider also fixes the FFI service id.
 *
 * Key-page URLs match `apps/desktop/src-tauri/src/commands/cloud.rs` (`KEY_PAGE_*`); the suggested
 * models match `v2c_core::route::suggested_models`. Keep them in sync with those two places.
 */
enum class CloudKind(
    val provider: CloudProvider,
    val serviceId: String,
    val label: String,
    val keyRequired: Boolean,
    val supportsAsr: Boolean,
    val keyPageUrl: String?,
    val suggestedModel: String,
    val suggestedAsrModel: String,
) {
    GEMINI(
        CloudProvider.GEMINI, "cloud-gemini", "Gemini", keyRequired = true, supportsAsr = true,
        keyPageUrl = "https://aistudio.google.com/apikey",
        suggestedModel = "gemini-3.6-flash", suggestedAsrModel = "gemini-3.6-flash",
    ),
    OPENAI(
        CloudProvider.OPENAI, "cloud-openai", "OpenAI", keyRequired = true, supportsAsr = true,
        keyPageUrl = "https://platform.openai.com/api-keys",
        suggestedModel = "gpt-6-luna", suggestedAsrModel = "gpt-transcribe",
    ),
    CLAUDE(
        CloudProvider.CLAUDE, "cloud-claude", "Claude", keyRequired = true, supportsAsr = false,
        keyPageUrl = "https://platform.claude.com/settings/keys",
        suggestedModel = "claude-haiku-5-5", suggestedAsrModel = "",
    ),
    CUSTOM(
        CloudProvider.CUSTOM, "cloud-custom", "自訂", keyRequired = false, supportsAsr = true,
        keyPageUrl = null,
        suggestedModel = "", suggestedAsrModel = "",
    ),
    ;

    companion object {
        fun parse(name: String?): CloudKind? = entries.firstOrNull { it.name == name }
    }
}

/**
 * One configured cloud service. Never holds the API key (that lives in [CloudKeyVault]), so the
 * generated `toString()` is safe to log.
 */
data class CloudServiceSettings(
    val kind: CloudKind,
    val model: String,
    val asrModel: String,
    val baseUrl: String,
    val uses: Set<CloudUse>,
) {
    val id: String get() = kind.serviceId

    fun toConfig(): CloudServiceConfig = CloudServiceConfig(
        id = id,
        provider = kind.provider,
        model = model.trim(),
        asrModel = asrModel.trim().ifEmpty { null },
        baseUrl = baseUrl.trim().ifEmpty { null },
        uses = uses.sorted(),
    )

    companion object {
        /** A new service pre-filled with the provider's suggested models. */
        fun suggested(kind: CloudKind): CloudServiceSettings = CloudServiceSettings(
            kind = kind,
            model = kind.suggestedModel,
            asrModel = kind.suggestedAsrModel,
            baseUrl = "",
            uses = if (kind.supportsAsr) setOf(CloudUse.ASR, CloudUse.TRANSLATE) else setOf(CloudUse.TRANSLATE),
        )
    }
}

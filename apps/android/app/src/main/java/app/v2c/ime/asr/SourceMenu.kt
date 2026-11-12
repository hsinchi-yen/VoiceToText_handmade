package app.v2c.ime.asr

import app.v2c.ime.ffi.AsrPick
import app.v2c.ime.ffi.AsrPlan
import app.v2c.ime.ffi.AsrSource

/** App-private preferences (SharedPreferences in production, a fake in tests). */
interface SourcePrefs {
    var pick: AsrPick

    /** True once the "download the local model?" question has been answered (asked once). */
    var localPromptAsked: Boolean
}

class InMemorySourcePrefs(
    override var pick: AsrPick = AsrPick.AUTO,
    override var localPromptAsked: Boolean = false,
) : SourcePrefs

/** What the keyboard should do besides updating the chip after a menu choice. */
enum class SelectEffect {
    NONE,

    /** Local model chosen but not downloaded and not yet asked: show the T2.6 download prompt. */
    SHOW_LOCAL_PROMPT,
}

/** Short label shown on the chip. */
enum class SourceLabel(val text: String) {
    TEA("TEA"), BREEZE("Breeze"), SYSTEM("系統"), LOCAL("本機"), NONE("無來源"), AUTO("自動")
}

/**
 * State of the recognition-source chip and its long-press menu. Immutable; the Compose layer
 * keeps one value in `mutableStateOf` and replaces it. Persists the pick through [SourcePrefs].
 */
data class SourceMenuState(
    val pick: AsrPick = AsrPick.AUTO,
    val menuOpen: Boolean = false,
) {
    fun openMenu(): SourceMenuState = copy(menuOpen = true)

    fun dismiss(): SourceMenuState = copy(menuOpen = false)

    /**
     * Applies a menu choice: closes the menu, stores it, and says whether the local-model
     * download question must be shown (first time LOCAL is chosen without a downloaded model).
     */
    fun select(
        newPick: AsrPick,
        prefs: SourcePrefs,
        localModelReady: Boolean,
    ): Pair<SourceMenuState, SelectEffect> {
        prefs.pick = newPick
        val effect = if (newPick == AsrPick.LOCAL && !localModelReady && !prefs.localPromptAsked) {
            SelectEffect.SHOW_LOCAL_PROMPT
        } else {
            SelectEffect.NONE
        }
        return copy(pick = newPick, menuOpen = false) to effect
    }

    companion object {
        /** Menu order (plan section 8.1): auto / TEA / Breeze / system / local. */
        val items: List<AsrPick> = listOf(AsrPick.AUTO, AsrPick.TEA, AsrPick.BREEZE, AsrPick.SYSTEM, AsrPick.LOCAL)

        /** Unknown or missing stored values mean AUTO. */
        fun parsePick(name: String?): AsrPick = AsrPick.entries.firstOrNull { it.name == name } ?: AsrPick.AUTO

        fun load(prefs: SourcePrefs) = SourceMenuState(pick = prefs.pick)

        /** Compact text for the inline menu buttons. */
        fun shortLabel(pick: AsrPick): String = when (pick) {
            AsrPick.AUTO -> "自動"
            AsrPick.TEA -> "TEA"
            AsrPick.BREEZE -> "Breeze"
            AsrPick.SYSTEM -> "系統"
            AsrPick.LOCAL -> "本機"
        }

        fun menuLabel(pick: AsrPick): String = when (pick) {
            AsrPick.AUTO -> "自動"
            AsrPick.TEA -> "區網 TEA"
            AsrPick.BREEZE -> "區網 Breeze"
            AsrPick.SYSTEM -> "系統辨識"
            AsrPick.LOCAL -> "本機模型"
        }

        /**
         * Chip text: the source that would be used now (auto) or the forced one. A forced source
         * that cannot be used still shows its own name; the reason is shown separately.
         */
        fun chipLabel(pick: AsrPick, plan: AsrPlan?): SourceLabel = when (pick) {
            AsrPick.TEA -> SourceLabel.TEA
            AsrPick.BREEZE -> SourceLabel.BREEZE
            AsrPick.SYSTEM -> SourceLabel.SYSTEM
            AsrPick.LOCAL -> SourceLabel.LOCAL
            AsrPick.AUTO -> when (plan?.primary) {
                AsrSource.LAN_TEA -> SourceLabel.TEA
                AsrSource.LAN_BREEZE -> SourceLabel.BREEZE
                AsrSource.SYSTEM -> SourceLabel.SYSTEM
                AsrSource.LOCAL -> SourceLabel.LOCAL
                AsrSource.NONE -> SourceLabel.NONE
                null -> SourceLabel.AUTO
            }
        }
    }
}

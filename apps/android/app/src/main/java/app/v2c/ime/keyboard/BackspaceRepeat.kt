package app.v2c.ime.keyboard

/**
 * Pure timing rules for holding the backspace key (no Android dependencies).
 *
 * A press deletes once immediately. If the key stays down, the first repeat comes after
 * [initialDelayMs], then repeats speed up from [startIntervalMs] down to [minIntervalMs].
 */
class BackspaceRepeat(
    private val initialDelayMs: Long = INITIAL_DELAY_MS,
    private val startIntervalMs: Long = START_INTERVAL_MS,
    private val minIntervalMs: Long = MIN_INTERVAL_MS,
    private val stepEvery: Int = STEP_EVERY,
    private val stepFactor: Double = STEP_FACTOR,
) {
    /** Delay to wait before repeat number [repeatIndex] (0 = first repeat after the initial press). */
    fun delayBeforeRepeat(repeatIndex: Int): Long {
        require(repeatIndex >= 0)
        if (repeatIndex == 0) return initialDelayMs
        var interval = startIntervalMs.toDouble()
        repeat((repeatIndex - 1) / stepEvery) { interval *= stepFactor }
        return interval.toLong().coerceAtLeast(minIntervalMs)
    }

    companion object {
        const val INITIAL_DELAY_MS = 400L
        const val START_INTERVAL_MS = 100L
        const val MIN_INTERVAL_MS = 30L
        const val STEP_EVERY = 5
        const val STEP_FACTOR = 0.7
    }
}

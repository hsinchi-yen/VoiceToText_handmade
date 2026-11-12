package app.v2c.ime

/**
 * Pure decision logic for the big microphone key (no Android dependencies).
 *
 * - Tap: press+release shorter than [holdThresholdMs] starts recording and keeps it running;
 *   the next press stops (and submits). The release that follows that press is swallowed.
 * - Hold: press held for at least [holdThresholdMs] is push-to-talk; the release stops.
 *
 * Timestamps are monotonic milliseconds supplied by the caller.
 */
class TapHoldDetector(private val holdThresholdMs: Long = HOLD_THRESHOLD_MS) {

    enum class Action { NONE, START, STOP }

    private var recording = false
    private var pressedAtMs = 0L
    private var swallowNextRelease = false

    fun onPress(nowMs: Long): Action {
        if (recording) {
            // Second tap of a tap-to-toggle session: stop now, ignore the matching release.
            recording = false
            swallowNextRelease = true
            return Action.STOP
        }
        recording = true
        pressedAtMs = nowMs
        swallowNextRelease = false
        return Action.START
    }

    fun onRelease(nowMs: Long): Action {
        if (swallowNextRelease) {
            swallowNextRelease = false
            return Action.NONE
        }
        if (!recording) return Action.NONE
        if (nowMs - pressedAtMs >= holdThresholdMs) {
            recording = false
            return Action.STOP
        }
        return Action.NONE
    }

    /** Call when the session ended for another reason (error, recognizer finished by itself). */
    fun reset() {
        recording = false
        swallowNextRelease = false
    }

    companion object {
        /** Press longer than this counts as push-to-talk (plan section 8). */
        const val HOLD_THRESHOLD_MS = 300L
    }
}

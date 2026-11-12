package app.v2c.ime.keyboard

import android.view.KeyEvent
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection

/** Applies backspace / enter to the focused editor. Never reads, touches or logs text content. */
class EditorKeys(
    private val connection: () -> InputConnection?,
    private val editorInfo: () -> EditorInfo?,
) {
    private var pressConnection: InputConnection? = null

    /**
     * Sends one backspace. [repeating] is false for the initial press and true for long-press ticks.
     * Returns false when a repeat should stop (no connection, or the connection changed since the press).
     */
    fun backspace(repeating: Boolean): Boolean {
        val ic = connection()
        if (ic == null) {
            pressConnection = null
            return false
        }
        if (!repeating) {
            pressConnection = ic
        } else if (ic !== pressConnection) {
            return false
        }
        // KEYCODE_DEL lets the editor delete a selection or a whole grapheme (ZWJ emoji, flags,
        // combining marks) itself, so the keyboard never reads field text.
        val sent = ic.sendKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL)) &&
            ic.sendKeyEvent(KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_DEL))
        if (!sent) ic.deleteSurroundingTextInCodePoints(1, 0)
        return true
    }

    fun enter() {
        val ic = connection() ?: return
        when (val action = decideEnterAction(editorInfo()?.imeOptions)) {
            is EnterAction.EditorAction -> ic.performEditorAction(action.actionId)
            EnterAction.InsertNewline -> ic.commitText("\n", 1)
        }
    }
}

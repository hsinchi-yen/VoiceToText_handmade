package app.v2c.ime.keyboard

import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection

/** Applies backspace / enter to the focused editor. Never touches or logs text content. */
class EditorKeys(
    private val connection: () -> InputConnection?,
    private val editorInfo: () -> EditorInfo?,
) {
    fun backspace() {
        val ic = connection() ?: return
        // Selection present: delete it. Otherwise delete one code point (keeps surrogate pairs intact).
        val selected = ic.getSelectedText(0)
        if (!selected.isNullOrEmpty()) {
            ic.commitText("", 1)
        } else {
            ic.deleteSurroundingTextInCodePoints(1, 0)
        }
    }

    fun enter() {
        val ic = connection() ?: return
        when (val action = decideEnterAction(editorInfo()?.imeOptions)) {
            is EnterAction.EditorAction -> ic.performEditorAction(action.actionId)
            EnterAction.InsertNewline -> ic.commitText("\n", 1)
        }
    }
}

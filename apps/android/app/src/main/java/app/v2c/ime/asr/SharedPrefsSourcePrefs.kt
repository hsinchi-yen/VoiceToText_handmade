package app.v2c.ime.asr

import android.content.Context
import app.v2c.ime.ffi.AsrPick

/** [SourcePrefs] in the app-private preferences file. Android glue. */
class SharedPrefsSourcePrefs(context: Context) : SourcePrefs {
    private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    override var pick: AsrPick
        get() = SourceMenuState.parsePick(prefs.getString(KEY_PICK, null))
        set(value) {
            prefs.edit().putString(KEY_PICK, value.name).apply()
        }

    override var localPromptAsked: Boolean
        get() = prefs.getBoolean(KEY_ASKED, false)
        set(value) {
            prefs.edit().putBoolean(KEY_ASKED, value).apply()
        }

    companion object {
        const val FILE = "v2c_source"
        const val KEY_PICK = "pick"
        const val KEY_ASKED = "local_prompt_asked"
    }
}

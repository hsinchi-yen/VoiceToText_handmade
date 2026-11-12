package app.v2c.ime.net

import java.net.HttpURLConnection
import java.net.URL

/**
 * Android/JVM [HttpProber] using HttpURLConnection. Any HTTP status below 500 counts as
 * "the server is there" (the model list may need a key we do not send). Cleartext to the two
 * LAN hosts is allowed by res/xml/network_security_config.xml.
 */
class HttpUrlConnectionProber : HttpProber {
    override fun get(url: String, timeoutMs: Int): Boolean {
        var conn: HttpURLConnection? = null
        return try {
            conn = (URL(url).openConnection() as HttpURLConnection).apply {
                connectTimeout = timeoutMs
                readTimeout = timeoutMs
                requestMethod = "GET"
                useCaches = false
                instanceFollowRedirects = false
            }
            conn.responseCode in 100..499
        } catch (_: Exception) {
            false
        } finally {
            conn?.disconnect()
        }
    }
}

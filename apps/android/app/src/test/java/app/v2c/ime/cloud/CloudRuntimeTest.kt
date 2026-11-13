package app.v2c.ime.cloud

import app.v2c.ime.FakeCipher
import app.v2c.ime.FakeFfi
import app.v2c.ime.ListLog
import app.v2c.ime.ffi.CloudUse
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CloudRuntimeTest {
    private val keys = InMemoryKeyValueStore()
    private val cipher = FakeCipher()
    private val repo = CloudRepository(InMemoryKeyValueStore(), CloudKeyVault(keys, cipher))
    private val ffi = FakeFfi()
    private val log = ListLog()
    private var validated = true
    private val runtime = CloudRuntime(repo, ffi, { validated }, log)

    private fun configureGemini(uses: Set<CloudUse> = setOf(CloudUse.ASR, CloudUse.TRANSLATE)) =
        repo.save(CloudServiceSettings.suggested(CloudKind.GEMINI).copy(uses = uses), "AIzaSECRETKEY1234")

    @Test
    fun nothingConfiguredIsNotReadyAndNeverCallsTheFfi() {
        assertTrue(runtime.reload())
        assertFalse(runtime.asrConfigured)
        assertFalse(runtime.asrReady())
        assertFalse(runtime.translateReady())
        assertNull(runtime.asrServiceId())
        assertTrue(ffi.cloudSets.isEmpty())
    }

    @Test
    fun readyNeedsTheServiceAReadableKeyAndAValidatedNetwork() {
        configureGemini()
        assertTrue(runtime.reload())
        assertTrue(runtime.asrConfigured)
        assertTrue(runtime.asrReady())
        assertEquals("cloud-gemini", runtime.asrServiceId())
        assertEquals("Gemini", runtime.asrLabel())
        assertTrue(runtime.translateReady())

        validated = false
        assertTrue(runtime.asrConfigured) // still configured, just not usable now
        assertFalse(runtime.asrReady())
        assertFalse(runtime.translateReady())

        validated = true
        cipher.broken = true // the key can no longer be decrypted
        runtime.reload()
        assertTrue(runtime.asrConfigured)
        assertFalse(runtime.asrReady())
    }

    @Test
    fun aFailedFfiLoadMeansNotReady() {
        configureGemini()
        ffi.setCloudFails = true
        assertFalse(runtime.reload())
        assertFalse(runtime.asrReady())
        assertTrue(log.lines.any { it.contains("failed") })
    }

    @Test
    fun reloadHandsServicesAndKeysToTheFfi() {
        configureGemini(setOf(CloudUse.ASR))
        runtime.reload()
        val (services, sentKeys) = ffi.cloudSets.single()
        assertEquals(listOf("cloud-gemini"), services.map { it.id })
        assertEquals(listOf(CloudUse.ASR), services.single().uses)
        assertEquals("AIzaSECRETKEY1234", sentKeys["cloud-gemini"])
    }

    @Test
    fun removingEverythingClearsTheFfiSet() {
        configureGemini()
        runtime.reload()
        repo.remove(CloudKind.GEMINI)
        runtime.reload()
        assertTrue(ffi.cloudSets.last().first.isEmpty())
        assertFalse(runtime.asrConfigured)
    }

    @Test
    fun aThrowingKeystoreDuringReloadIsNotReadyAndDoesNotThrow() {
        configureGemini()
        cipher.throwing = true
        assertTrue(runtime.reload()) // keys unreadable, services still pushed
        assertTrue(runtime.asrConfigured)
        assertFalse(runtime.asrReady())
    }

    @Test
    fun aThrowingRepositoryMeansNotReady() {
        val badStore = object : KeyValueStore {
            override fun get(key: String): String? = throw IllegalStateException("storage")
            override fun put(key: String, value: String) = Unit
            override fun remove(key: String) = Unit
        }
        val broken = CloudRuntime(CloudRepository(badStore, CloudKeyVault(keys, cipher)), ffi, { true }, log)
        assertFalse(broken.reload())
        assertFalse(broken.asrReady())
    }

    @Test
    fun forcedReloadClearsATestedButUnsavedDraft() {
        // Nothing saved and never pushed: a plain reload sends nothing ...
        runtime.reload()
        assertTrue(ffi.cloudSets.isEmpty())
        // ... but after a draft test the empty set must reach the FFI.
        runtime.reload(force = true)
        assertTrue(ffi.cloudSets.single().first.isEmpty())
        assertTrue(ffi.cloudSets.single().second.isEmpty())
    }

    @Test
    fun logsNeverContainTheKey() {
        configureGemini()
        runtime.reload()
        assertTrue(log.lines.isNotEmpty())
        assertTrue(log.lines.none { it.contains("SECRETKEY") })
    }
}

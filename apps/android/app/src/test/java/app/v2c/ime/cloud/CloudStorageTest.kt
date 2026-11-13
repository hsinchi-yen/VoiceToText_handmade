package app.v2c.ime.cloud

import app.v2c.ime.FakeCipher
import app.v2c.ime.ffi.CloudUse
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CloudStorageTest {
    private val settings = InMemoryKeyValueStore()
    private val keys = InMemoryKeyValueStore()
    private val cipher = FakeCipher()
    private val repo = CloudRepository(settings, CloudKeyVault(keys, cipher))

    private fun gemini(vararg uses: CloudUse) =
        CloudServiceSettings.suggested(CloudKind.GEMINI).copy(uses = uses.toSet())

    @Test
    fun theKeyIsNeverStoredInPlainText() {
        repo.save(gemini(CloudUse.ASR), "AIzaSECRETKEY1234")
        val everything = (settings.map.values + keys.map.values + settings.map.keys + keys.map.keys).joinToString("|")
        assertFalse(everything.contains("AIzaSECRETKEY1234"))
        assertFalse(everything.contains("SECRETKEY"))
        assertTrue(keys.map.isNotEmpty())
        // Settings hold no key material at all.
        assertTrue(settings.map.none { it.key.startsWith("key.") })
    }

    @Test
    fun theKeyRoundTripsAndOnlyTheLastFourAreShown() {
        repo.save(gemini(CloudUse.ASR), "  AIzaSECRETKEY1234 ")
        assertEquals("AIzaSECRETKEY1234", repo.snapshot().keysForFfi()["cloud-gemini"])
        assertEquals("1234", repo.lastFour(CloudKind.GEMINI))
        assertEquals("••••1234", CloudSetupLogic.maskedKey(repo.lastFour(CloudKind.GEMINI)))
    }

    @Test
    fun savingWithABlankKeyKeepsTheStoredKey() {
        repo.save(gemini(CloudUse.ASR), "KEY-ONE-9999")
        repo.save(gemini(CloudUse.TRANSLATE), "")
        assertEquals("9999", repo.lastFour(CloudKind.GEMINI))
        assertEquals(setOf(CloudUse.TRANSLATE), repo.service(CloudKind.GEMINI)!!.uses)
    }

    @Test
    fun anUnreadableKeyIsDroppedFromTheSnapshot() {
        repo.save(gemini(CloudUse.ASR), "KEY-ONE-9999")
        cipher.broken = true
        val snap = repo.snapshot()
        assertTrue(snap.keysForFfi().isEmpty())
        assertNull(snap.readyServiceFor(CloudUse.ASR))
        assertEquals(CloudKind.GEMINI, snap.serviceFor(CloudUse.ASR)!!.kind)
    }

    @Test
    fun onlyOneServicePerUseTheNewestWins() {
        repo.save(gemini(CloudUse.ASR, CloudUse.TRANSLATE), "k1")
        repo.save(CloudServiceSettings.suggested(CloudKind.OPENAI).copy(uses = setOf(CloudUse.ASR)), "k2")
        assertEquals(setOf(CloudUse.TRANSLATE), repo.service(CloudKind.GEMINI)!!.uses)
        assertEquals(setOf(CloudUse.ASR), repo.service(CloudKind.OPENAI)!!.uses)
        assertEquals(CloudKind.OPENAI, repo.snapshot().serviceFor(CloudUse.ASR)!!.kind)
        assertEquals(CloudKind.GEMINI, repo.snapshot().serviceFor(CloudUse.TRANSLATE)!!.kind)
    }

    @Test
    fun claudeCanOnlyTranslate() {
        repo.save(
            CloudServiceSettings.suggested(CloudKind.CLAUDE).copy(uses = setOf(CloudUse.ASR, CloudUse.TRANSLATE)),
            "k",
        )
        assertEquals(setOf(CloudUse.TRANSLATE), repo.service(CloudKind.CLAUDE)!!.uses)
        assertEquals(setOf(CloudUse.TRANSLATE), CloudSetupLogic.allowedUses(CloudKind.CLAUDE))
    }

    @Test
    fun removingAServiceRemovesItsKeyToo() {
        repo.save(gemini(CloudUse.ASR), "KEY-ONE-9999")
        repo.remove(CloudKind.GEMINI)
        assertTrue(repo.services().isEmpty())
        assertFalse(repo.hasKey(CloudKind.GEMINI))
        assertTrue(keys.map.isEmpty())
    }

    @Test
    fun aCustomEndpointNeedsNoKey() {
        val custom = CloudServiceSettings.suggested(CloudKind.CUSTOM)
            .copy(model = "m", asrModel = "w", baseUrl = "https://example.com/v1", uses = setOf(CloudUse.ASR))
        repo.save(custom, null)
        assertEquals(CloudKind.CUSTOM, repo.snapshot().readyServiceFor(CloudUse.ASR)!!.kind)
    }

    @Test
    fun aThrowingKeystoreOnSaveReportsFailureAndStoresNothing() {
        cipher.throwing = true
        assertFalse(repo.save(gemini(CloudUse.ASR), "KEY-ONE-9999"))
        assertTrue(repo.services().isEmpty())
        assertTrue(keys.map.isEmpty())
        // Without a new key nothing is encrypted, so saving settings still works.
        assertTrue(repo.save(gemini(CloudUse.ASR), null))
    }

    @Test
    fun aThrowingKeystoreOnReadMeansNoKeyInsteadOfACrash() {
        repo.save(gemini(CloudUse.ASR), "KEY-ONE-9999")
        cipher.throwing = true
        val snap = repo.snapshot()
        assertTrue(snap.keysForFfi().isEmpty())
        assertNull(snap.readyServiceFor(CloudUse.ASR))
        assertNull(repo.lastFour(CloudKind.GEMINI))
    }

    @Test
    fun toStringNeverContainsAKey() {
        repo.save(gemini(CloudUse.ASR), "AIzaSECRETKEY1234")
        assertFalse(repo.snapshot().toString().contains("SECRETKEY"))
        assertFalse(repo.services().toString().contains("SECRETKEY"))
        assertFalse(CloudKeyVault(keys, cipher).toString().contains("SECRETKEY"))
    }
}

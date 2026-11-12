package app.hghub.mobile.data

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.io.IOException
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Backups contain ciphertext only; the Android Keystore key stays on this device. */
internal class TokenVault(context: Context) {
    private val prefs = context.getSharedPreferences("hghub_session_v2", Context.MODE_PRIVATE)
    private val alias = context.packageName + ".session.v2"

    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(alias, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        generator.init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())
        return generator.generateKey()
    }

    fun read(origin: String): String = runCatching {
        val raw = Base64.decode(prefs.getString("token", ""), Base64.NO_WRAP)
        if (raw.size <= 28) return@runCatching ""
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, raw.copyOfRange(0, 12)))
        cipher.updateAAD(origin.toByteArray(Charsets.UTF_8))
        cipher.doFinal(raw.copyOfRange(12, raw.size)).toString(Charsets.UTF_8)
    }.getOrDefault("")

    fun write(token: String, origin: String) {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key())
        cipher.updateAAD(origin.toByteArray(Charsets.UTF_8))
        val raw = cipher.iv + cipher.doFinal(token.toByteArray(Charsets.UTF_8))
        if (!prefs.edit().putString("token", Base64.encodeToString(raw, Base64.NO_WRAP)).commit()) throw IOException("无法保存会话")
    }

    fun clear() { prefs.edit().clear().apply() }
}

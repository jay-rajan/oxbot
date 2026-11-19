package dev.oxbot.openrecord

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.webkit.MimeTypeMap
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin
import java.io.File

@InvokeArg
class OpenArgs {
    lateinit var path: String
}

// Opens a kept record in the phone's own app for its kind of file. A copy
// goes in the app's cache (the only folder shared), handed over read-only.
@TauriPlugin
class OpenRecordPlugin(private val activity: Activity) : Plugin(activity) {
    @Command
    fun open(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(OpenArgs::class.java)
            val source = File(args.path)
            val folder = File(activity.cacheDir, "open-record")
            folder.deleteRecursively()
            folder.mkdirs()
            val shared = File(folder, source.name)
            source.copyTo(shared, overwrite = true)
            val uri = FileProvider.getUriForFile(activity, activity.packageName + ".records", shared)
            val ext = shared.extension.lowercase()
            val type = MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext) ?: "*/*"
            val view = Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, type)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            activity.startActivity(Intent.createChooser(view, "Open with"))
            invoke.resolve()
        } catch (e: ActivityNotFoundException) {
            invoke.reject("No app on this phone opens this kind of file.")
        } catch (e: Exception) {
            invoke.reject(e.message ?: "The file couldn't be opened.")
        }
    }
}

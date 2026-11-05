package ink.penroam.nativeprint

import android.app.Activity
import android.content.Context
import android.os.Bundle
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.print.PageRange
import android.print.PrintAttributes
import android.print.PrintDocumentAdapter
import android.print.PrintDocumentInfo
import android.print.PrintManager
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.FileOutputStream
import java.io.IOException

@InvokeArg
class PrintArgs {
    lateinit var path: String
    lateinit var title: String
    var pages: Int = PrintDocumentInfo.PAGE_COUNT_UNKNOWN
}

/**
 * Sends a finished PDF to Android's print UI. Page ranges, copies and the printer are all chosen
 * there, and the print spooler cuts the document down to the chosen pages itself.
 *
 * Reachable only from the app's Rust side (see the plugin's `lib.rs`). A device without a print
 * spooler fails with an error message; a device with no print service shows only "Save as PDF".
 */
@TauriPlugin
class NativePrintPlugin(private val activity: Activity) : Plugin(activity) {

    /** Resolves as soon as the print UI opens: Android does not report how that UI ends. */
    @Command
    fun printPdf(invoke: Invoke) {
        val args = invoke.parseArgs(PrintArgs::class.java)
        val file = File(args.path)
        activity.runOnUiThread {
            try {
                val manager = activity.getSystemService(Context.PRINT_SERVICE) as? PrintManager
                if (manager == null) {
                    file.delete()
                    invoke.reject("This device has no print service.")
                    return@runOnUiThread
                }
                manager.print(args.title, PdfAdapter(file, args.title, args.pages), null)
                invoke.resolve(JSObject().put("status", "systemDialog"))
            } catch (t: RuntimeException) {
                file.delete()
                invoke.reject("Printing is not available on this device: ${t.message}")
            }
        }
    }
}

/** Serves one PDF to the print spooler and deletes it when the print UI is done with it. */
private class PdfAdapter(
    private val file: File,
    private val title: String,
    private val pages: Int,
) : PrintDocumentAdapter() {

    override fun onLayout(
        oldAttributes: PrintAttributes?,
        newAttributes: PrintAttributes,
        cancellationSignal: CancellationSignal?,
        callback: LayoutResultCallback,
        extras: Bundle?,
    ) {
        if (cancellationSignal?.isCanceled == true) {
            callback.onLayoutCancelled()
            return
        }
        val info = PrintDocumentInfo.Builder(title)
            .setContentType(PrintDocumentInfo.CONTENT_TYPE_DOCUMENT)
            .setPageCount(pages)
            .build()
        // The file never depends on the chosen paper, so one write is enough.
        callback.onLayoutFinished(info, oldAttributes == null)
    }

    override fun onWrite(
        pages: Array<out PageRange>,
        destination: ParcelFileDescriptor,
        cancellationSignal: CancellationSignal?,
        callback: WriteResultCallback,
    ) {
        try {
            file.inputStream().use { input ->
                FileOutputStream(destination.fileDescriptor).use { input.copyTo(it) }
            }
            if (cancellationSignal?.isCanceled == true) callback.onWriteCancelled()
            else callback.onWriteFinished(arrayOf(PageRange.ALL_PAGES))
        } catch (e: IOException) {
            callback.onWriteFailed(e.message)
        }
    }

    override fun onFinish() {
        file.delete()
    }
}

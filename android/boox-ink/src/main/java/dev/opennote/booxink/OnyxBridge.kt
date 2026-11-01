package dev.opennote.booxink

import android.graphics.Rect
import android.graphics.RectF
import android.view.MotionEvent
import android.view.View
import com.onyx.android.sdk.api.device.epd.EpdController
import com.onyx.android.sdk.api.device.epd.UpdateMode
import com.onyx.android.sdk.data.note.TouchPoint
import com.onyx.android.sdk.device.Device
import com.onyx.android.sdk.pen.RawInputCallback
import com.onyx.android.sdk.pen.TouchHelper
import com.onyx.android.sdk.pen.data.TouchPointList

/**
 * The only file that references Onyx SDK classes. Everything else in the
 * module talks to [OnyxRawSession] and [OnyxBridge], so a missing or broken
 * SDK surfaces as a catchable Throwable at a single boundary instead of a
 * crash in unrelated code.
 */
internal object OnyxBridge {
    fun deviceIndex(): String = Device.currentDeviceIndex().name
    fun maxTouchPressure(): Float = EpdController.getMaxTouchPressure()

    fun refreshRegionGc(view: View, r: Rect) =
        EpdController.invalidate(view, r.left, r.top, r.right, r.bottom, UpdateMode.GC)

    fun handwritingRepaint(view: View) {
        EpdController.invalidate(view, UpdateMode.HAND_WRITING_REPAINT_MODE)
        EpdController.refreshScreen(view, UpdateMode.HAND_WRITING_REPAINT_MODE)
    }

    fun fullRefreshGc(view: View) = EpdController.invalidate(view, UpdateMode.GC)

    fun setHandwritingRegion(view: View) = EpdController.setScreenHandWritingRegionLimit(view)

    fun toRaw(tp: TouchPoint): RawSample =
        RawSample(tp.x, tp.y, tp.pressure, tp.tiltX.toFloat(), tp.tiltY.toFloat(), tp.timestamp)

    fun toRaw(list: TouchPointList?): List<RawSample> =
        list?.points?.map { toRaw(it) } ?: emptyList()
}

/**
 * Wraps one TouchHelper raw-drawing session. All calls must be made on the
 * owning [BooxInkView]'s session executor; callbacks arrive on the SDK's
 * reader thread and are forwarded to [sink] unchanged.
 */
internal class OnyxRawSession(
    private val view: View,
    private val sink: Sink,
    featureAllCreate: Boolean,
) {
    interface Sink {
        fun onBegin(eraser: Boolean, s: RawSample?)
        fun onMove(eraser: Boolean, s: RawSample)
        fun onList(eraser: Boolean, samples: List<RawSample>)
        fun onEnd(eraser: Boolean, s: RawSample?)
        fun onPenUpRefresh(rect: RectF?)
        fun onPenActive()
    }

    // Parameters are nullable on purpose: the SDK is Java and some firmware
    // delivers null points (Boox-EinkDraw guards the same way).
    private val callback = object : RawInputCallback() {
        override fun onBeginRawDrawing(b: Boolean, tp: TouchPoint?) = sink.onBegin(false, tp?.let { OnyxBridge.toRaw(it) })
        override fun onEndRawDrawing(b: Boolean, tp: TouchPoint?) = sink.onEnd(false, tp?.let { OnyxBridge.toRaw(it) })
        override fun onRawDrawingTouchPointMoveReceived(tp: TouchPoint?) { tp?.let { sink.onMove(false, OnyxBridge.toRaw(it)) } }
        override fun onRawDrawingTouchPointListReceived(l: TouchPointList?) = sink.onList(false, OnyxBridge.toRaw(l))
        override fun onBeginRawErasing(b: Boolean, tp: TouchPoint?) = sink.onBegin(true, tp?.let { OnyxBridge.toRaw(it) })
        override fun onEndRawErasing(b: Boolean, tp: TouchPoint?) = sink.onEnd(true, tp?.let { OnyxBridge.toRaw(it) })
        override fun onRawErasingTouchPointMoveReceived(tp: TouchPoint?) { tp?.let { sink.onMove(true, OnyxBridge.toRaw(it)) } }
        override fun onRawErasingTouchPointListReceived(l: TouchPointList?) = sink.onList(true, OnyxBridge.toRaw(l))
        override fun onPenUpRefresh(rect: RectF?) = sink.onPenUpRefresh(rect)
        override fun onPenActive(tp: TouchPoint?) = sink.onPenActive()
    }

    // Two creation variants are exposed because firmware differs:
    //  - documented:  TouchHelper.create(view, callback)  (Onyx-Pen-SDK.md)
    //  - featureAll:  TouchHelper.create(view, FEATURE_ALL_TOUCH_RENDER, callback, false)
    //                 (used by Boox-EinkDraw on Note Air4C / Android 13)
    private val helper: TouchHelper = if (featureAllCreate) {
        TouchHelper.create(view, TouchHelper.FEATURE_ALL_TOUCH_RENDER, callback, false)
    } else {
        TouchHelper.create(view, callback)
    }

    val created: Boolean get() = helper.isRawDrawingCreated

    /**
     * Order follows the sequence verified by Boox-EinkDraw: width/color,
     * limit, open (resets the pen engine), then style and params again.
     */
    fun open(limit: Rect, exclude: List<Rect>, widthPx: Float, pressureStyle: Boolean, renderEnabled: Boolean, color: Int = android.graphics.Color.BLACK) {
        helper.setStrokeWidth(widthPx)
        helper.setStrokeColor(color)
        helper.setLimitRect(limit, exclude)
        helper.openRawDrawing()
        applyStyle(widthPx, pressureStyle, color)
        helper.setRawDrawingRenderEnabled(renderEnabled)
    }

    fun applyStyle(widthPx: Float, pressureStyle: Boolean, color: Int = android.graphics.Color.BLACK) {
        helper.setStrokeStyle(if (pressureStyle) TouchHelper.STROKE_STYLE_FOUNTAIN else TouchHelper.STROKE_STYLE_PENCIL)
        helper.setStrokeWidth(widthPx)
        helper.setStrokeColor(color)
    }

    fun setLimit(limit: Rect, exclude: List<Rect>) { helper.setLimitRect(limit, exclude) }
    fun setEnabled(enabled: Boolean) { helper.setRawDrawingEnabled(enabled) }
    fun setRenderEnabled(enabled: Boolean) { helper.setRawDrawingRenderEnabled(enabled) }
    val inputEnabled: Boolean get() = helper.isRawDrawingInputEnabled
    val renderEnabled: Boolean get() = helper.isRawDrawingRenderEnabled
    fun onTouchEvent(e: MotionEvent): Boolean = helper.onTouchEvent(e)

    fun close() {
        try { helper.setRawDrawingEnabled(false) } catch (_: Throwable) {}
        try { helper.setRawInputReaderEnable(false) } catch (_: Throwable) {}
        helper.closeRawDrawing()
    }
}

package dev.opennote.m00.m00_flutter_app

import dev.opennote.booxink.BooxInk
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        BooxInk.init(application)
        flutterEngine.platformViewsController.registry.registerViewFactory(
            BooxPlatformView.VIEW_TYPE,
            BooxPlatformViewFactory(flutterEngine.dartExecutor.binaryMessenger),
        )
    }
}

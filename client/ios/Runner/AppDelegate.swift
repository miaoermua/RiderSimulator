import AudioToolbox
import Flutter
import UIKit

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)

    guard let registrar = engineBridge.pluginRegistry.registrar(forPlugin: "AlertSoundPlugin")
    else { return }
    let channel = FlutterMethodChannel(
      name: "net.miaoer.game.crowdsource/alert",
      binaryMessenger: registrar.messenger()
    )
    channel.setMethodCallHandler { call, result in
      switch call.method {
      case "playDefaultNotification":
        // 1007 是系统「三全音」，也是本地通知不指定 sound 时的默认音。
        // 静音拨片拨下时系统不响，跟系统通知的行为一致。
        AudioServicesPlaySystemSound(1007)
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }
}

package net.miaoer.game.crowdsource

import android.media.AudioAttributes
import android.media.Ringtone
import android.media.RingtoneManager
import android.os.Handler
import android.os.Looper
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    private var playing: Ringtone? = null
    private val stopHandler = Handler(Looper.getMainLooper())

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, CHANNEL)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "playDefaultNotification" -> {
                        playDefaultNotification()
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
    }

    /**
     * 播一次系统「默认通知铃声」，对应设置里的「系统默认」。
     *
     * 不让通知渠道自己响是因为：渠道音在建渠道时定死，换不了用户选的本地文件，
     * 而且渠道响铃 + 应用内播自定义会双响。走这里则跟系统设置完全一致——
     * 用户换铃声、调音量、开勿扰，行为都跟着系统走。
     */
    private fun playDefaultNotification() {
        val uri = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_NOTIFICATION) ?: return
        val ringtone = RingtoneManager.getRingtone(applicationContext, uri) ?: return
        ringtone.audioAttributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_NOTIFICATION)
            .build()

        playing?.stop()
        playing = ringtone
        ringtone.play()

        // 通知音正常只有一两秒；用户要是设成了一首歌，别让它一直盖着界面。
        stopHandler.removeCallbacksAndMessages(null)
        stopHandler.postDelayed({ runCatching { ringtone.stop() } }, 2000)
    }

    override fun onDestroy() {
        stopHandler.removeCallbacksAndMessages(null)
        playing?.stop()
        super.onDestroy()
    }

    companion object {
        private const val CHANNEL = "net.miaoer.game.crowdsource/alert"
    }
}

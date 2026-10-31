import 'dart:async';

import 'package:audioplayers/audioplayers.dart';
import 'package:file_picker/file_picker.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'alert_event.dart';
import 'alert_sound_store.dart';

/// 来单 / 活动 / 上下线的唯一出口：系统通知 + 应用内提示音。
///
/// **为什么音效不交给通知渠道**：Android 8+ 的 `NotificationChannel` 音效
/// 只能在建渠道那一刻定死，之后 App 改不了（要换只能换新的 channel id），
/// 而且只能引用打进 `res/raw` 的资源，指不了用户自己选的音频。所以这里
/// 把音效统一交给应用内：系统渠道一律 `playSound: false`，避免同一条提醒
/// 响两声，也让「换音效」不用动渠道。
///
/// 默认音是**系统自带的默认通知铃声**（Android 走 RingtoneManager、
/// iOS 走 SystemSound 1007，见平台通道），不打包任何音频进 APK；
/// 用户可以在设置里给「指派单 / 上线 / 下线」各换一个本地文件。
///
/// 代价：进程被杀之后没有声音。但那条路上 WebSocket 也早断了，本来收不到
/// 新单事件；真要覆盖杀进程场景只能另接厂商推送（见 docs 里的讨论）。
class AlertCenter extends ChangeNotifier {
  AlertCenter._();

  static final AlertCenter instance = AlertCenter._();

  static const _soundPrefsKey = 'alert_sound_enabled';
  static const _slotKeyPrefix = 'alert_sound_';
  static const _assignChannelId = 'rider_assign';
  static const _activityChannelId = 'rider_activity';

  /// 固定 id：同一类提醒只占一条通知，新的覆盖旧的，不在状态栏堆一串。
  static const _assignNotificationId = 1001;
  static const _activityNotificationId = 1002;

  /// 播系统默认通知音的通道，实现在 MainActivity.kt / AppDelegate.swift。
  static const _soundChannel =
      MethodChannel('net.miaoer.game.crowdsource/alert');

  final FlutterLocalNotificationsPlugin _notifications =
      FlutterLocalNotificationsPlugin();
  final AudioPlayer _player = AudioPlayer();

  /// 每个槽自定义文件路径；`null` = 用系统默认音。
  final Map<AlertSoundSlot, String?> _customPaths = {};

  bool _ready = false;
  bool _soundEnabled = true;

  /// 提示音总开关。设置页可改，存本地偏好（跟账号无关）。
  bool get soundEnabled => _soundEnabled;

  /// 能不能选自定义音频。浏览器没有可持久化的私有目录，直接不给入口。
  static bool get canPickCustomSound => !kIsWeb;

  /// 只有真机才有系统通知。Web / 桌面端退化成「只响音效」。
  static bool get _hasNotifications =>
      !kIsWeb &&
      (defaultTargetPlatform == TargetPlatform.android ||
          defaultTargetPlatform == TargetPlatform.iOS);

  /// 启动时调一次。任何一步失败都只记一行日志——提醒不是主链路，
  /// 插件没注册、权限被拒都不该让 App 起不来。
  Future<void> init() async {
    if (_ready) return;
    _ready = true;

    try {
      await _player.setReleaseMode(ReleaseMode.stop);
    } catch (e) {
      debugPrint('alert: 音频初始化失败 $e');
    }

    try {
      final prefs = await SharedPreferences.getInstance();
      _soundEnabled = prefs.getBool(_soundPrefsKey) ?? true;
      for (final slot in AlertSoundSlot.values) {
        final path = prefs.getString('$_slotKeyPrefix${slot.name}');
        if (path != null && path.isNotEmpty) _customPaths[slot] = path;
      }
      notifyListeners();
    } catch (e) {
      debugPrint('alert: 读提示音设置失败 $e');
    }

    if (!_hasNotifications) return;

    try {
      await _notifications.initialize(
        settings: const InitializationSettings(
          // 单色小图标，见 android/app/src/main/res/drawable/ic_stat_order.xml。
          android: AndroidInitializationSettings('ic_stat_order'),
          iOS: DarwinInitializationSettings(
            // 权限在 init 末尾统一要，这里不重复弹。
            requestAlertPermission: false,
            requestBadgePermission: false,
            requestSoundPermission: false,
            // App 在前台时也把横幅放出来：骑手可能正盯着地图边缘，
            // 顶部横幅比页面里的弹窗更早被看见。
            defaultPresentAlert: true,
            defaultPresentBanner: true,
            defaultPresentSound: false,
          ),
        ),
      );
      await _createChannels();
      await _requestPermissions();
    } catch (e) {
      debugPrint('alert: 通知初始化失败 $e');
    }
  }

  Future<void> _createChannels() async {
    final android = _notifications.resolvePlatformSpecificImplementation<
        AndroidFlutterLocalNotificationsPlugin>();
    if (android == null) return;

    await android.createNotificationChannel(
      const AndroidNotificationChannel(
        _assignChannelId,
        '来单提醒',
        description: '指派单与强制派单',
        importance: Importance.max,
        // 音效由应用内播；渠道自带铃声会变成双响。
        playSound: false,
        enableVibration: true,
      ),
    );
    await android.createNotificationChannel(
      const AndroidNotificationChannel(
        _activityChannelId,
        '平台活动',
        description: '新活动上架',
        importance: Importance.defaultImportance,
        playSound: false,
        enableVibration: false,
      ),
    );
  }

  Future<void> _requestPermissions() async {
    if (defaultTargetPlatform == TargetPlatform.android) {
      // Android 13+ 的 POST_NOTIFICATIONS。旧系统上是空操作。
      await _notifications
          .resolvePlatformSpecificImplementation<
              AndroidFlutterLocalNotificationsPlugin>()
          ?.requestNotificationsPermission();
    } else if (defaultTargetPlatform == TargetPlatform.iOS) {
      await _notifications
          .resolvePlatformSpecificImplementation<
              IOSFlutterLocalNotificationsPlugin>()
          ?.requestPermissions(alert: true, badge: true, sound: true);
    }
  }

  /// 弹一条提醒：先响音效，再出通知。
  ///
  /// 顺序是有意的——音效是本机的，几十毫秒就出来；通知要过系统那一层，
  /// 慢的时候要等一两百毫秒。骑手先听到声，再看屏幕。
  Future<void> show(RiderAlert alert) async {
    unawaited(_play(alert.soundSlot));
    if (!alert.showsNotification || !_hasNotifications) return;
    try {
      final isActivity = alert.kind == AlertKind.activity;
      await _notifications.show(
        id: isActivity ? _activityNotificationId : _assignNotificationId,
        title: alert.title,
        body: alert.body,
        notificationDetails: NotificationDetails(
          android: AndroidNotificationDetails(
            isActivity ? _activityChannelId : _assignChannelId,
            isActivity ? '平台活动' : '来单提醒',
            icon: 'ic_stat_order',
            importance:
                isActivity ? Importance.defaultImportance : Importance.max,
            priority: isActivity ? Priority.defaultPriority : Priority.high,
            playSound: false,
            enableVibration: !isActivity,
            // 指派单用 message 类别：部分 ROM 会按类别给不同的横幅/悬浮策略。
            category: isActivity
                ? AndroidNotificationCategory.recommendation
                : AndroidNotificationCategory.message,
            autoCancel: true,
            onlyAlertOnce: false,
          ),
          iOS: const DarwinNotificationDetails(
            presentAlert: true,
            presentBanner: true,
            presentSound: false,
          ),
        ),
      );
    } catch (e) {
      debugPrint('alert: 弹通知失败 $e');
    }
  }

  /// 某个槽当前的自定义文件路径，`null` = 系统默认音。
  String? customPath(AlertSoundSlot slot) => _customPaths[slot];

  /// 设置页副标题用的展示名。
  String soundLabel(AlertSoundSlot slot) {
    final path = _customPaths[slot];
    if (path == null || path.isEmpty) return '系统默认通知音';
    final name = path.split(RegExp(r'[/\\]')).last;
    return name.isEmpty ? '自定义音频' : name;
  }

  /// 试听（设置页用）：不管总开关状态，直接响一次。
  Future<void> preview(AlertSoundSlot slot) => _play(slot, force: true);

  /// 让用户从系统文件选择器挑一段音频。
  ///
  /// 挑完立刻试听——不给试听的话，用户只能等到真来单才知道选错了。
  Future<void> pickSound(AlertSoundSlot slot) async {
    if (!canPickCustomSound) return;
    try {
      final file = await FilePicker.pickFile(
        type: FileType.audio,
        dialogTitle: '选择提示音',
      );
      if (file == null) return;
      final saved = await saveSoundFile(slot, file);
      if (saved == null) return;

      final old = _customPaths[slot];
      if (old != null && old != saved) await deleteSoundFile(old);
      _customPaths[slot] = saved;
      await _persist();
      notifyListeners();
      await preview(slot);
    } catch (e) {
      debugPrint('alert: 选提示音失败 $e');
    }
  }

  /// 恢复成系统默认通知音。
  Future<void> resetSound(AlertSoundSlot slot) async {
    final old = _customPaths[slot];
    if (old == null) return;
    _customPaths[slot] = null;
    await _persist();
    notifyListeners();
    await deleteSoundFile(old);
  }

  Future<void> setSoundEnabled(bool on) async {
    if (_soundEnabled == on) return;
    _soundEnabled = on;
    notifyListeners();
    if (!on) {
      try {
        await _player.stop();
      } catch (_) {
        // 没在播就没什么可停的。
      }
    }
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setBool(_soundPrefsKey, on);
    } catch (e) {
      debugPrint('alert: 存提示音设置失败 $e');
    }
  }

  Future<void> _play(AlertSoundSlot? slot, {bool force = false}) async {
    if (!force && !_soundEnabled) return;

    // 两条提醒挨着来（比如连派两单），后一条打断前一条，不叠着响。
    try {
      await _player.stop();
    } catch (_) {
      // 播放器还没初始化完，忽略。
    }

    final path = slot == null ? null : _customPaths[slot];
    if (path != null && slot != null) {
      try {
        await _player.play(DeviceFileSource(path));
        return;
      } catch (e) {
        // 文件被删 / 格式不支持：退回系统音，并把失效配置清掉。
        debugPrint('alert: 自定义音效播不了，退回系统音 $e');
        _customPaths[slot] = null;
        unawaited(_persist());
        notifyListeners();
      }
    }
    await _playSystemSound();
  }

  /// 播系统自带的默认通知铃声。
  Future<void> _playSystemSound() async {
    if (kIsWeb) return;
    try {
      await _soundChannel.invokeMethod<void>('playDefaultNotification');
    } on MissingPluginException {
      // 桌面平台没实现，当静音处理。
    } on PlatformException catch (e) {
      debugPrint('alert: 系统提示音播放失败 ${e.message}');
    }
  }

  Future<void> _persist() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      for (final slot in AlertSoundSlot.values) {
        final path = _customPaths[slot];
        if (path == null) {
          await prefs.remove('$_slotKeyPrefix${slot.name}');
        } else {
          await prefs.setString('$_slotKeyPrefix${slot.name}', path);
        }
      }
    } catch (e) {
      debugPrint('alert: 存提示音设置失败 $e');
    }
  }
}

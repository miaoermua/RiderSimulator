import 'dart:async';
import 'dart:io' show Platform;

import 'package:flutter/foundation.dart';
import 'package:geolocator/geolocator.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'game_client.dart';
import 'game_state.dart';
import 'map/coord.dart';

/// 定位刷新频率。**省电是默认值**——跑单时手机一直亮着，定位是最耗电的一项。
enum LocationRate {
  /// 不请求定位，也不上报。
  off('关闭', Duration.zero, 0),

  /// 一分钟一次、走出 200 米才更新。
  saver('省电', Duration(seconds: 60), 200),

  /// 半分钟一次、走出 100 米才更新。
  normal('标准', Duration(seconds: 30), 100),

  /// 十秒一次、走出 30 米才更新。演示 / 调试用。
  high('高频', Duration(seconds: 10), 30);

  const LocationRate(this.label, this.interval, this.meters);

  final String label;

  /// 两次上报之间的最小间隔（Android 直接交给系统节流，其余平台自己掐）。
  final Duration interval;

  /// 距离过滤：原地不动时不产生新定位，省电的关键。
  final int meters;

  String get detail => this == LocationRate.off
      ? '不请求定位，也不上报'
      : '${interval.inSeconds} 秒 / $meters 米';

  static LocationRate byName(String? name) => values.firstWhere(
        (r) => r.name == name,
        orElse: () => LocationRate.normal,
      );
}

/// 定位 → 上报 → 本地更新「我」的位置。
///
/// 只做一件事：拿到手机 GPS（WGS-84），换成世界坐标（GCJ-02），
/// 发给服务端校准，同时把本地 `me` 挪过去。服务端的 tick 帧不带骑手坐标，
/// 不补这一步，地图上的「我」和订单距离会停在连接那一刻。
class LocationService extends ChangeNotifier {
  LocationService._();

  static final LocationService instance = LocationService._();

  static const _prefsKey = 'location_rate';

  GameClient? _client;
  GameState? _state;
  StreamSubscription<Position>? _sub;

  /// 每次重启定位自增，旧的一轮回来时据此丢弃（设置里连点不会留下两条流）。
  int _generation = 0;

  LocationRate _rate = LocationRate.normal;
  String? _error;
  DateTime? _lastSentAt;
  DateTime? _lastFixAt;
  double? _lastLat;
  double? _lastLng;

  LocationRate get rate => _rate;

  /// 定位流开着（不等于已经拿到过坐标）。
  bool get running => _sub != null;

  /// 最近一次失败原因；成功一次后清空。
  String? get error => _error;

  DateTime? get lastFixAt => _lastFixAt;

  /// 最近一次上报出去的世界坐标。
  double? get lastLat => _lastLat;
  double? get lastLng => _lastLng;

  /// 从本地设置里恢复上次选的频率。
  Future<void> loadSaved() async {
    final prefs = await SharedPreferences.getInstance();
    _rate = LocationRate.byName(prefs.getString(_prefsKey));
    notifyListeners();
    // 读设置和进主界面是两条异步路径，谁先到都可能：已经绑定过就按新频率重开。
    if (_client != null) await _apply();
  }

  /// 登录成功、进入主界面后调用。重复调用只会重绑定，不会叠出第二条流。
  Future<void> attach(GameClient client, GameState state) async {
    _client = client;
    _state = state;
    await _apply();
  }

  Future<void> setRate(LocationRate rate) async {
    _rate = rate;
    notifyListeners();
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(_prefsKey, rate.name);
    await _apply();
  }

  Future<void> _apply() async {
    final gen = ++_generation;
    await _sub?.cancel();
    _sub = null;
    _error = null;
    notifyListeners();

    if (_rate == LocationRate.off || _client == null) return;

    try {
      if (!await Geolocator.isLocationServiceEnabled()) {
        _fail(gen, '系统定位服务没打开，去手机设置里开一下');
        return;
      }
      var permission = await Geolocator.checkPermission();
      if (permission == LocationPermission.denied) {
        permission = await Geolocator.requestPermission();
      }
      if (gen != _generation) return;
      if (permission == LocationPermission.denied ||
          permission == LocationPermission.deniedForever) {
        _fail(
          gen,
          permission == LocationPermission.deniedForever
              ? '定位权限已被永久拒绝，去系统设置里允许'
              : '没有定位权限',
        );
        return;
      }

      _sub = Geolocator.getPositionStream(locationSettings: _settings())
          .listen(_onFix, onError: (Object e) => _fail(gen, '定位失败：$e'));
      notifyListeners();
    } catch (e) {
      _fail(gen, '定位启动失败：$e');
    }
  }

  void _fail(int gen, String message) {
    if (gen != _generation) return;
    _error = message;
    notifyListeners();
  }

  LocationSettings _settings() {
    if (!kIsWeb && Platform.isAndroid) {
      // intervalDuration 是硬节流；distanceFilter 让站着不动时干脆不醒。
      return AndroidSettings(
        accuracy: LocationAccuracy.medium,
        distanceFilter: _rate.meters,
        intervalDuration: _rate.interval,
      );
    }
    if (!kIsWeb && Platform.isIOS) {
      return AppleSettings(
        accuracy: LocationAccuracy.medium,
        distanceFilter: _rate.meters,
        // 前台跑单才定位：后台定位要在 Info.plist 里开 background modes，
        // 也会让系统弹「始终允许」，对省电和审核都不划算。
        allowBackgroundLocationUpdates: false,
        pauseLocationUpdatesAutomatically: true,
      );
    }
    return LocationSettings(
      accuracy: LocationAccuracy.medium,
      distanceFilter: _rate.meters,
    );
  }

  void _onFix(Position p) {
    // 距离过滤之外再掐一次时间：部分机型会忽略 intervalDuration，
    // 骑行时抖一下就来一条，电量和流量都白花。
    final now = DateTime.now();
    final last = _lastSentAt;
    if (last != null && now.difference(last) < _rate.interval) return;

    final world = wgs84ToGcj02(p.latitude, p.longitude);
    _lastSentAt = now;
    _lastFixAt = now;
    _lastLat = world.latitude;
    _lastLng = world.longitude;
    _error = null;

    _client?.sendPosition(world.latitude, world.longitude);
    _state?.applyLocalFix(world.latitude, world.longitude);
    notifyListeners();
  }
}

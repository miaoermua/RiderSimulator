import 'dart:io' show Platform;

import 'package:flutter/foundation.dart';
import 'package:latlong2/latlong.dart';
import 'package:url_launcher/url_launcher.dart';

import 'map/coord.dart';
import 'protocol.dart';

/// 导航目标：取货点还是送货点。
///
/// 单独一个枚举是为了让调用方**必须**说清楚导的是哪一头——
/// 取货用商家坐标、送货用顾客坐标，两者搞混会让骑手骑到反方向去。
enum NavTarget {
  pickup('取货'),
  dropoff('送货');

  const NavTarget(this.label);
  final String label;
}

/// 可选的导航应用。
///
/// 顺序就是设置页里的展示顺序。`system` 永远排第一且永远可用。
enum NavApp {
  system('系统默认'),
  apple('Apple 地图'),
  amap('高德地图'),
  baidu('百度地图'),
  google('Google Maps');

  const NavApp(this.label);
  final String label;

  /// 这个平台能列出哪些选项。
  ///
  /// **不列出来做不到的**：Android 上没有 Apple 地图，硬列一个点了没反应
  /// 的选项比不给更糟。Web 只给「系统默认」（实际是开网页版地图）。
  static List<NavApp> forPlatform() {
    if (kIsWeb) return const [NavApp.system];
    if (Platform.isIOS) return values;
    if (Platform.isAndroid) {
      return const [NavApp.system, NavApp.amap, NavApp.baidu, NavApp.google];
    }
    return const [NavApp.system];
  }

  static NavApp parse(String? raw) =>
      values.firstWhere((a) => a.name == raw, orElse: () => NavApp.system);
}

/// 一次导航尝试的结果。给调用方决定弹什么提示。
enum NavOutcome {
  /// 已经拉起某个地图应用。
  launched,

  /// 坐标缺失（0 或者是 NaN/越界）。
  noCoords,

  /// 目标应用没装，且回退链也没成功。
  unavailable,

  /// 全都试过了，系统拒绝启动。
  failed,
}

/// 导航服务：选应用、算坐标、拼 URI、按回退链尝试拉起。
///
/// 集中在这里的原因：URI 拼接和平台判断有十来处分支，散在订单卡里
/// 会变成一个既难读又没法单测的巨块。卡片只负责说"导这一头"。
class NavigationService {
  NavigationService._();

  /// 坐标是否可用。
  ///
  /// `(0, 0)` 判为**不可用**——它是大西洋上的一个点，本项目里不会出现
  /// 真实订单落在那里，出现就说明是缺字段被默认值填了。
  /// 这一条与 `sim::dispatch` 的口径不同（那边 `(0,0)` 合法），
  /// 因为这里关乎"把人导到海里去"，宁可误判。
  static bool coordsUsable(double lat, double lng) {
    if (!lat.isFinite || !lng.isFinite) return false;
    if (lat.abs() < 1e-9 && lng.abs() < 1e-9) return false;
    return lat.abs() <= 90 && lng.abs() <= 180;
  }

  /// 按目标应用把快照里的坐标换成它期望的坐标系。
  ///
  /// - 高德 / 百度 / Apple（中国大陆用高德数据）/ 系统默认 → GCJ-02
  /// - Google → WGS-84
  ///
  /// 快照坐标的坐标系由服务端 `coord_system` 决定，**不能假设**。
  static LatLng coordFor(
    NavApp app,
    double lat,
    double lng,
    CoordSystem from,
  ) {
    final needWgs84 = app == NavApp.google;
    if (from == CoordSystem.wgs84) {
      return needWgs84 ? LatLng(lat, lng) : wgs84ToGcj02(lat, lng);
    }
    return needWgs84 ? gcj02ToWgs84(lat, lng) : LatLng(lat, lng);
  }

  /// 拼一个候选 URI。**纯函数、不发起调用**，单测直接断言这里的输出。
  ///
  /// 返回 `null` 表示这个 (平台, 应用) 组合没有可用 scheme——
  /// 调用方会跳过它去试下一个。
  static Uri? buildUri({
    required NavApp app,
    required double lat,
    required double lng,
    required String name,
    required bool isIOS,
    required bool isAndroid,
  }) {
    final label = Uri.encodeComponent(name);

    if (isIOS) {
      switch (app) {
        case NavApp.system:
        case NavApp.apple:
          // Apple 地图的官方导航 URL：`dirflg=d` 表示驾车。
          // 用 http（不是 https）是苹果文档给的写法，能直接唤起 App。
          return Uri.parse('http://maps.apple.com/?daddr=$lat,$lng&dirflg=d');
        case NavApp.amap:
          // 高德 iOS scheme：`dev=0` 表示传入的是 GCJ-02 坐标。
          return Uri.parse(
            'iosamap://navi?sourceApplication=rider_sim&poiname=$label'
            '&lat=$lat&lon=$lng&dev=0&style=2',
          );
        case NavApp.baidu:
          // `coord_type=gcj02` 让百度自己做 BD-09 换算，我们只管给 GCJ-02。
          return Uri.parse(
            'baidumap://map/direction?destination=latlng:$lat,$lng|name:$label'
            '&coord_type=gcj02&mode=driving&src=ios.rider_sim',
          );
        case NavApp.google:
          return Uri.parse(
            'comgooglemaps://?daddr=$lat,$lng&directionsmode=driving',
          );
      }
    }

    if (isAndroid) {
      switch (app) {
        case NavApp.system:
          // `geo:` 交给系统选择器：装了哪个地图就列哪个。
          // `q=lat,lng(name)` 比纯 `geo:lat,lng` 多带一个名字，部分地图会显示。
          return Uri.parse('geo:$lat,$lng?q=$lat,$lng($label)');
        case NavApp.amap:
          // `style=2` 是驾车导航；`dev=0` 表示入参为 GCJ-02。
          return Uri.parse(
            'androidamap://navi?sourceApplication=rider_sim&poiname=$label'
            '&lat=$lat&lon=$lng&dev=0&style=2',
          );
        case NavApp.baidu:
          return Uri.parse(
            'baidumap://map/direction?destination=latlng:$lat,$lng|name:$label'
            '&coord_type=gcj02&mode=driving&src=andr.rider_sim',
          );
        case NavApp.google:
          // 走 Google 的导航 Intent：直接进导航态，不是只打开地图。
          return Uri.parse('google.navigation:q=$lat,$lng');
        case NavApp.apple:
          // Android 上没有 Apple 地图。
          return null;
      }
    }

    return null;
  }

  /// 网页兜底：所有本地 scheme 都失败时用。
  ///
  /// 选高德网页版而不是 Google Maps：后者在国内打不开，
  /// 拿它当兜底等于没有兜底。
  static Uri webFallback(double lat, double lng, String name) => Uri.parse(
        'https://uri.amap.com/navigation'
        '?to=$lng,$lat,${Uri.encodeComponent(name)}&mode=car&coordinate=gaode',
      );

  /// 回退顺序：**选中应用 → 系统默认 → 网页版**。
  ///
  /// 抽成纯函数是为了能单测"某应用不可用时会不会继续往下走"——
  /// 真实拉起要依赖设备上装了哪个 App，CI 里测不了。
  ///
  /// 选中「系统默认」时不再重复，否则会连着试两遍同一个 scheme。
  static List<NavApp> fallbackChain(NavApp selected) {
    final chain = <NavApp>[selected];
    if (selected != NavApp.system) chain.add(NavApp.system);
    return chain;
  }

  /// 拉起导航。
  ///
  /// 回退顺序：**选中应用 → 系统默认（若与选中不同）→ 网页版**。
  /// 任何一步成功就返回；全失败返回 `unavailable` / `failed`。
  /// **不会因为某个应用没装就中断整个流程**——那正是最容易踩的坑。
  static Future<NavOutcome> navigate({
    required NavApp app,
    required double lat,
    required double lng,
    required String name,
    required CoordSystem coordSystem,
  }) async {
    if (!coordsUsable(lat, lng)) return NavOutcome.noCoords;

    final isIOS = !kIsWeb && Platform.isIOS;
    final isAndroid = !kIsWeb && Platform.isAndroid;

    final chain = fallbackChain(app);

    var triedAny = false;
    for (final candidate in chain) {
      final point = coordFor(candidate, lat, lng, coordSystem);
      final uri = buildUri(
        app: candidate,
        lat: point.latitude,
        lng: point.longitude,
        name: name,
        isIOS: isIOS,
        isAndroid: isAndroid,
      );
      if (uri == null) continue;
      triedAny = true;
      if (await _tryLaunch(uri)) return NavOutcome.launched;
    }

    // 网页兜底。坐标统一按 GCJ-02 给高德网页版。
    final gcj = coordSystem == CoordSystem.gcj02
        ? LatLng(lat, lng)
        : wgs84ToGcj02(lat, lng);
    if (await _tryLaunch(webFallback(gcj.latitude, gcj.longitude, name))) {
      return NavOutcome.launched;
    }

    return triedAny ? NavOutcome.unavailable : NavOutcome.failed;
  }

  /// 试一个 URI。**先 `canLaunchUrl` 再 `launchUrl`**：
  /// 直接 launch 一个没装应用的 scheme 在部分 Android 机型上会抛
  /// `PlatformException`，打断整条回退链。
  static Future<bool> _tryLaunch(Uri uri) async {
    try {
      if (!await canLaunchUrl(uri)) return false;
      return await launchUrl(uri, mode: LaunchMode.externalApplication);
    } catch (e) {
      debugPrint('navigation: 启动 $uri 失败 $e');
      return false;
    }
  }
}

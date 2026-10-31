import 'package:flutter_test/flutter_test.dart';
import 'package:rider_client/map/coord.dart';
import 'package:rider_client/navigation_service.dart';
import 'package:rider_client/protocol.dart';

/// 导航服务的纯逻辑测试。
///
/// 只测"给定输入产出什么 URI / 什么坐标"——**不碰真实拉起**：
/// CI 机器上不会装高德或百度，任何依赖 `url_launcher` 真跑的断言都不稳定。
/// 拉起本身的行为放在真机验收里，见最终报告。
void main() {
  // 武汉光谷一带的真实量级坐标。
  const lat = 30.5928;
  const lng = 114.3055;

  group('坐标可用性', () {
    test('正常坐标可用', () {
      expect(NavigationService.coordsUsable(lat, lng), isTrue);
    });

    test('(0,0) 视为缺失，不能当目的地', () {
      // 这是"缺字段被默认值填了"的典型形态，导过去是大西洋。
      expect(NavigationService.coordsUsable(0, 0), isFalse);
    });

    test('越界与 NaN 不可用', () {
      expect(NavigationService.coordsUsable(91, 114), isFalse);
      expect(NavigationService.coordsUsable(30, 181), isFalse);
      expect(NavigationService.coordsUsable(double.nan, 114), isFalse);
      expect(NavigationService.coordsUsable(30, double.infinity), isFalse);
    });
  });

  group('坐标系换算', () {
    test('GCJ-02 交给高德：原样不动', () {
      final p =
          NavigationService.coordFor(NavApp.amap, lat, lng, CoordSystem.gcj02);
      expect(p.latitude, closeTo(lat, 1e-9));
      expect(p.longitude, closeTo(lng, 1e-9));
    });

    test('GCJ-02 交给 Google：转成 WGS-84，偏移确实变小', () {
      final p = NavigationService.coordFor(
          NavApp.google, lat, lng, CoordSystem.gcj02);
      // 中国境内两者差几百米（纬度方向约 0.001~0.005 度）。
      expect(p.latitude, isNot(closeTo(lat, 1e-9)));
      // 但不会差到"另一个城市"：容差 0.02 度（约 2km）。
      expect((p.latitude - lat).abs(), lessThan(0.02));
      expect((p.longitude - lng).abs(), lessThan(0.02));
    });

    test('WGS-84 交给高德：转成 GCJ-02', () {
      final p =
          NavigationService.coordFor(NavApp.amap, lat, lng, CoordSystem.wgs84);
      expect(p.latitude, isNot(closeTo(lat, 1e-9)));
    });

    test('WGS-84 交给 Google：原样不动', () {
      final p = NavigationService.coordFor(
          NavApp.google, lat, lng, CoordSystem.wgs84);
      expect(p.latitude, closeTo(lat, 1e-9));
      expect(p.longitude, closeTo(lng, 1e-9));
    });

    test('正反变换可以来回还原（1 米内）', () {
      final wgs = gcj02ToWgs84(lat, lng);
      final back = wgs84ToGcj02(wgs.latitude, wgs.longitude);
      // 1e-5 度约 1.1 米，迭代两轮足够。
      expect(back.latitude, closeTo(lat, 1e-5));
      expect(back.longitude, closeTo(lng, 1e-5));
    });
  });

  group('URI 构造 · Android', () {
    Uri build(NavApp app, {double la = lat, double ln = lng}) =>
        NavigationService.buildUri(
          app: app,
          lat: la,
          lng: ln,
          name: '张记热干面',
          isIOS: false,
          isAndroid: true,
        )!;

    test('系统默认走 geo:，带名称', () {
      final u = build(NavApp.system);
      expect(u.scheme, 'geo');
      expect(u.path, '$lat,$lng');
      expect(u.query, contains('$lat,$lng'));
    });

    test('高德走 androidamap://navi，含导航参数', () {
      final u = build(NavApp.amap);
      expect(u.scheme, 'androidamap');
      expect(u.host, 'navi');
      // dev=0 表示入参是 GCJ-02；style=2 是驾车导航。
      expect(u.queryParameters['dev'], '0');
      expect(u.queryParameters['style'], '2');
      expect(u.queryParameters['lat'], '$lat');
      expect(u.queryParameters['lon'], '$lng');
    });

    test('百度走 baidumap://map/direction，声明 gcj02', () {
      final u = build(NavApp.baidu);
      expect(u.scheme, 'baidumap');
      expect(u.queryParameters['coord_type'], 'gcj02');
      expect(u.queryParameters['mode'], 'driving');
      expect(u.queryParameters['destination'], contains('$lat,$lng'));
    });

    test('Google 走 google.navigation:q=lat,lng（直接进导航）', () {
      final u = build(NavApp.google);
      expect(u.toString(), 'google.navigation:q=$lat,$lng');
    });

    test('Android 上没有 Apple 地图，返回 null 而不是拼个假 URI', () {
      final u = NavigationService.buildUri(
        app: NavApp.apple,
        lat: lat,
        lng: lng,
        name: 'x',
        isIOS: false,
        isAndroid: true,
      );
      expect(u, isNull);
    });
  });

  group('URI 构造 · iOS', () {
    Uri? build(NavApp app) => NavigationService.buildUri(
          app: app,
          lat: lat,
          lng: lng,
          name: '张记热干面',
          isIOS: true,
          isAndroid: false,
        );

    test('Apple 地图用 maps.apple.com + dirflg=d', () {
      final u = build(NavApp.apple)!;
      expect(u.host, 'maps.apple.com');
      expect(u.queryParameters['daddr'], '$lat,$lng');
      expect(u.queryParameters['dirflg'], 'd');
    });

    test('系统默认等价于 Apple 地图', () {
      expect(build(NavApp.system).toString(), build(NavApp.apple).toString());
    });

    test('高德用 iosamap://navi', () {
      final u = build(NavApp.amap)!;
      expect(u.scheme, 'iosamap');
      expect(u.queryParameters['dev'], '0');
    });

    test('百度用 baidumap://map/direction', () {
      final u = build(NavApp.baidu)!;
      expect(u.scheme, 'baidumap');
      expect(u.queryParameters['coord_type'], 'gcj02');
    });

    test('Google 用 comgooglemaps://', () {
      final u = build(NavApp.google)!;
      expect(u.scheme, 'comgooglemaps');
      expect(u.queryParameters['directionsmode'], 'driving');
    });
  });

  group('回退链', () {
    test('选了第三方：先第三方、再系统默认', () {
      expect(NavigationService.fallbackChain(NavApp.amap),
          [NavApp.amap, NavApp.system]);
    });

    test('选了系统默认：不重复', () {
      expect(NavigationService.fallbackChain(NavApp.system), [NavApp.system]);
    });

    test('网页兜底指向高德网页版（国内可达）', () {
      final u = NavigationService.webFallback(lat, lng, '张记');
      expect(u.toString(), startsWith('https://uri.amap.com/navigation'));
      // 高德的坐标顺序是「经度,纬度」。
      expect(u.queryParameters['to'], startsWith('$lng,$lat'));
    });
  });

  group('导航应用枚举', () {
    test('能从字符串还原，未知值退回系统默认', () {
      expect(NavApp.parse('amap'), NavApp.amap);
      expect(NavApp.parse('baidu'), NavApp.baidu);
      expect(NavApp.parse(null), NavApp.system);
      expect(NavApp.parse('nope'), NavApp.system);
    });

    test('每个平台至少给一个可用项，且都包含系统默认', () {
      final list = NavApp.forPlatform();
      expect(list, isNotEmpty);
      expect(list.first, NavApp.system);
    });
  });
}

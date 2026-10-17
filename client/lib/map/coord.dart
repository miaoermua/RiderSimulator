import 'dart:math' as math;

import 'package:latlong2/latlong.dart';

/// WGS-84 → GCJ-02 偏移。
///
/// 手机 GPS、OSM、Google 给的都是 WGS-84；服务端的世界坐标与高德底图是
/// GCJ-02。在中国境内两者差几百米——**直接上报会让骑手落到隔壁街区**，
/// 所以每次上报前都必须过一遍这里。
///
/// 算法是公开的 GCJ-02 加密公式（各地 SDK 通用），只在境内生效：
/// 境外原样返回，免得在东京 / 旧金山反而被挪走。
LatLng wgs84ToGcj02(double lat, double lng) {
  if (!inChina(lat, lng)) return LatLng(lat, lng);

  var dLat = _transformLat(lng - 105.0, lat - 35.0);
  var dLng = _transformLng(lng - 105.0, lat - 35.0);

  final radLat = lat / 180.0 * math.pi;
  var magic = math.sin(radLat);
  magic = 1 - _ee * magic * magic;
  final sqrtMagic = math.sqrt(magic);

  dLat = (dLat * 180.0) /
      ((_a * (1 - _ee)) / (magic * sqrtMagic) * math.pi);
  dLng = (dLng * 180.0) /
      (_a / sqrtMagic * math.cos(radLat) * math.pi);

  return LatLng(lat + dLat, lng + dLng);
}

/// 粗略的「是否在中国境内」判断，够用来决定要不要做偏移。
bool inChina(double lat, double lng) =>
    lng > 73.66 && lng < 135.05 && lat > 3.86 && lat < 53.55;

const double _a = 6378245.0;
const double _ee = 0.00669342162296594323;

double _transformLat(double x, double y) {
  var ret = -100.0 +
      2.0 * x +
      3.0 * y +
      0.2 * y * y +
      0.1 * x * y +
      0.2 * math.sqrt(x.abs());
  ret += (20.0 * math.sin(6.0 * x * math.pi) +
          20.0 * math.sin(2.0 * x * math.pi)) *
      2.0 /
      3.0;
  ret += (20.0 * math.sin(y * math.pi) +
          40.0 * math.sin(y / 3.0 * math.pi)) *
      2.0 /
      3.0;
  ret += (160.0 * math.sin(y / 12.0 * math.pi) +
          320 * math.sin(y * math.pi / 30.0)) *
      2.0 /
      3.0;
  return ret;
}

double _transformLng(double x, double y) {
  var ret = 300.0 +
      x +
      2.0 * y +
      0.1 * x * x +
      0.1 * x * y +
      0.1 * math.sqrt(x.abs());
  ret += (20.0 * math.sin(6.0 * x * math.pi) +
          20.0 * math.sin(2.0 * x * math.pi)) *
      2.0 /
      3.0;
  ret += (20.0 * math.sin(x * math.pi) +
          40.0 * math.sin(x / 3.0 * math.pi)) *
      2.0 /
      3.0;
  ret += (150.0 * math.sin(x / 12.0 * math.pi) +
          300.0 * math.sin(x / 30.0 * math.pi)) *
      2.0 /
      3.0;
  return ret;
}

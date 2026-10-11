import '../protocol.dart';

/// 底图瓦片源。
///
/// 坐标系决定用哪家底图——这是「切换坐标系」必须**跟着换底图**的原因：
/// GCJ-02 的坐标叠在 WGS-84 的底图上，在中国境内会整体偏移几百米。
///
/// 与烽火台 `server/console/src/basemaps.ts` 一一对应（同一份瓦片地址），
/// 改一处要记得改另一处。
///
/// | 坐标系 | 底图 | 授权 |
/// |---|---|---|
/// | GCJ-02 | 高德网页瓦片 | 未授权直连，仅本地演示用 |
/// | WGS-84 | OpenStreetMap | ODbL，需遵守使用条款（限速、规范 UA） |
class Basemap {
  const Basemap({
    required this.id,
    required this.label,
    required this.urlTemplate,
    required this.subdomains,
    required this.attribution,
    required this.maxZoom,
  });

  final CoordSystem id;
  final String label;
  final String urlTemplate;
  final List<String> subdomains;
  final String attribution;
  final int maxZoom;
}

const Map<CoordSystem, Basemap> basemaps = {
  CoordSystem.gcj02: Basemap(
    id: CoordSystem.gcj02,
    label: '高德',
    urlTemplate:
        'https://webrd0{s}.is.autonavi.com/appmaptile?lang=zh_cn&size=1&scale=1&style=8&x={x}&y={y}&z={z}',
    subdomains: ['1', '2', '3', '4'],
    attribution: '© 高德地图',
    maxZoom: 18,
  ),
  CoordSystem.wgs84: Basemap(
    id: CoordSystem.wgs84,
    label: 'OpenStreetMap',
    urlTemplate: 'https://tile.openstreetmap.org/{z}/{x}/{y}.png',
    subdomains: ['a', 'b', 'c'],
    attribution: '© OpenStreetMap contributors',
    maxZoom: 19,
  ),
};

/// 按坐标系取底图。未知值退回高德（服务端默认 GCJ-02）。
Basemap basemapFor(CoordSystem cs) => basemaps[cs] ?? basemaps[CoordSystem.gcj02]!;

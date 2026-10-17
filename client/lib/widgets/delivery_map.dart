import 'package:flutter/material.dart';
import 'package:flutter_map/flutter_map.dart';
import 'package:latlong2/latlong.dart';

import '../game_state.dart';
import '../map/basemaps.dart';
import '../protocol.dart';

/// 地图的兜底视角：武汉光谷，与 `sim::world` 的商圈中心一致。
///
/// 拿到点位后会立刻框选过去，这里只是防止首帧闪到几百公里外。
const _fallbackCenter = LatLng(30.5077, 114.4154);

/// 订单连线的颜色按状态分——和订单卡上的状态语义一致。
const _orderLineColor = <OrderStatus, Color>{
  OrderStatus.pending: Color(0xFF1976D2),
  OrderStatus.assigned: Color(0xFFF57C00),
  OrderStatus.pickedUp: Color(0xFF2E7D32),
};

/// 通用地图：底图 + 商户 + 订单取送连线 + 可选的我。
///
/// 底图默认跟随服务端下发的坐标系（GCJ-02 → 高德，WGS-84 → OSM），
/// 也可以在图上手动选。两者不一致时顶部会挂一条橙色提示——
/// GCJ-02 的点叠在 OSM 底图上会整体偏几百米，不能让人默默看错坐标。
///
/// 与烽火台 `MapView.tsx` 同一套底图与配色，只是把 Leaflet 换成了
/// `flutter_map`（同一族 XYZ 瓦片，不需要高德原生 SDK 与 Key）。
class DeliveryMap extends StatefulWidget {
  const DeliveryMap({
    super.key,
    required this.state,
    this.orders = const [],
    this.showMerchants = true,
    this.showMe = false,
    this.showBasemapPicker = true,
    this.fitPadding = 56,
    this.focus,
    this.arcRoutes = false,
  });

  final GameState state;

  /// 要高亮画线的订单。为空时只画底图（与商户，如果开着）。
  final List<OrderView> orders;
  final bool showMerchants;
  final bool showMe;
  final bool showBasemapPicker;
  final double fitPadding;

  /// 直接聚焦到某个点（例如订单卡上切到「取」或「送」）。
  ///
  /// 非空时首帧不做框选，后续变化会平移过去——切点的预期就是「镜头跟过去」，
  /// 而不是每切一次都重新框一遍整单。
  final LatLng? focus;

  /// 取送之间画圆弧而不是直线。
  ///
  /// 多单叠在一张图上时，直线会糊成一团；弧线各走一边，能看出哪条属于哪一单。
  /// 单张订单卡里仍用直线——那里只需要「这两点连着」。
  final bool arcRoutes;

  @override
  State<DeliveryMap> createState() => _DeliveryMapState();
}

class _DeliveryMapState extends State<DeliveryMap> {
  final MapController _map = MapController();

  /// 首屏框选只做一次：之后不再抢用户拖出来的视角。
  /// 订单是后来才同步到的，所以失败的（点位为空）不算数。
  bool _fitted = false;

  @override
  void didUpdateWidget(covariant DeliveryMap old) {
    super.didUpdateWidget(old);
    final f = widget.focus;
    if (f == null || f == old.focus) return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _map.move(f, 16);
    });
  }

  @override
  void dispose() {
    _map.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final state = widget.state;
    final bm = basemapFor(state.basemap);
    final me = state.me;

    // 订单的取送两端优先；没有订单时才退到「看全城商户」。
    final orderPoints = [
      if (widget.showMe && me != null) LatLng(me.lat, me.lng),
      for (final o in widget.orders) ...[
        LatLng(o.merchantLat, o.merchantLng),
        LatLng(o.customerLat, o.customerLng),
      ],
    ];
    final fitPoints = orderPoints.isNotEmpty
        ? orderPoints
        : [
            if (widget.showMerchants)
              for (final m in state.merchants) LatLng(m.lat, m.lng),
          ];

    _scheduleFit(fitPoints);

    return Stack(
      children: [
        FlutterMap(
          mapController: _map,
          options: MapOptions(
            initialCenter:
                fitPoints.isEmpty ? _fallbackCenter : fitPoints.first,
            initialZoom: 14,
            minZoom: 3,
            maxZoom: 19,
            interactionOptions: const InteractionOptions(
              // 双指旋转对骑手没用，还容易把地图转歪，关掉。
              flags: InteractiveFlag.all & ~InteractiveFlag.rotate,
            ),
          ),
          children: [
            TileLayer(
              // 换底图必须换图层，key 变了才会重新拉瓦片。
              key: ValueKey(bm.id),
              urlTemplate: bm.urlTemplate,
              subdomains: bm.subdomains,
              maxZoom: bm.maxZoom.toDouble(),
              userAgentPackageName: 'net.miaoer.game.crowdsource',
            ),
            if (widget.showMerchants) _merchantLayer(state),
            if (widget.orders.isNotEmpty) _orderLineLayer(),
            if (widget.orders.isNotEmpty) _orderMarkerLayer(),
            if (widget.showMe && me != null) _meLayer(me),
          ],
        ),
        if (state.basemapMismatch)
          Positioned(
            left: 0,
            right: 0,
            top: 0,
            child: _MismatchBar(basemap: bm, coord: state.config.coordSystem),
          ),
        Positioned(
          right: 10,
          bottom: 10,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              _MapButton(
                icon: Icons.center_focus_strong,
                tooltip: '回到我的单',
                onTap: () {
                  if (fitPoints.isEmpty) return;
                  _fit(fitPoints);
                },
              ),
              if (widget.showBasemapPicker) ...[
                const SizedBox(height: 8),
                _MapButton(
                  icon: Icons.layers_outlined,
                  tooltip: '切换底图',
                  onTap: () => _pickBasemap(context),
                ),
              ],
            ],
          ),
        ),
        Positioned(
          left: 8,
          bottom: 8,
          child: _Attribution(text: '${bm.label} · ${bm.attribution}'),
        ),
      ],
    );
  }

  // ---------------------------------------------------------------------
  // 图层
  // ---------------------------------------------------------------------

  Widget _merchantLayer(GameState state) => CircleLayer(
        circles: [
          for (final m in state.merchants)
            CircleMarker(
              point: LatLng(m.lat, m.lng),
              radius: 4,
              color: Colors.blueGrey.withValues(alpha: 0.45),
              borderColor: Colors.blueGrey,
              borderStrokeWidth: 1,
            ),
        ],
      );

  Widget _orderLineLayer() => PolylineLayer(
        polylines: [
          for (final o in widget.orders)
            Polyline(
              points: widget.arcRoutes
                  ? _arcBetween(
                      LatLng(o.merchantLat, o.merchantLng),
                      LatLng(o.customerLat, o.customerLng),
                    )
                  : [
                      LatLng(o.merchantLat, o.merchantLng),
                      LatLng(o.customerLat, o.customerLng),
                    ],
              color: _orderLineColor[o.status] ?? Colors.deepOrange,
              strokeWidth: 3,
              // 还没接的单画虚线：路线是「可能会走」，不是「正在走」。
              // 注意 `dashed` 内部要读 segments.length，整条不能是 const。
              pattern: o.status == OrderStatus.pending
                  ? StrokePattern.dashed(segments: const [8, 6])
                  : const StrokePattern.solid(),
            ),
        ],
      );

  Widget _orderMarkerLayer() => MarkerLayer(
        markers: [
          for (final o in widget.orders) ...[
            Marker(
              point: LatLng(o.merchantLat, o.merchantLng),
              width: 26,
              height: 26,
              alignment: Alignment.center,
              child: const _PinDot(label: '取', color: Color(0xFF1976D2)),
            ),
            Marker(
              point: LatLng(o.customerLat, o.customerLng),
              width: 26,
              height: 26,
              alignment: Alignment.center,
              child: const _PinDot(label: '送', color: Color(0xFF2E7D32)),
            ),
          ],
        ],
      );

  Widget _meLayer(RiderView me) => MarkerLayer(
        markers: [
          Marker(
            point: LatLng(me.lat, me.lng),
            width: 30,
            height: 30,
            alignment: Alignment.center,
            child: Container(
              decoration: BoxDecoration(
                color: Colors.blue.shade600,
                shape: BoxShape.circle,
                border: Border.all(color: Colors.white, width: 2),
              ),
              alignment: Alignment.center,
              child: const Icon(Icons.delivery_dining, size: 16, color: Colors.white),
            ),
          ),
        ],
      );

  // ---------------------------------------------------------------------
  // 视角
  // ---------------------------------------------------------------------

  void _scheduleFit(List<LatLng> points) {
    if (_fitted) return;
    final focus = widget.focus;
    if (focus != null) {
      _fitted = true;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _map.move(focus, 16);
      });
      return;
    }
    if (points.isEmpty) return;
    _fitted = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _fit(points);
    });
  }

  void _fit(List<LatLng> points) {
    if (points.length == 1) {
      _map.move(points.first, 16);
      return;
    }
    _map.fitCamera(
      CameraFit.bounds(
        bounds: LatLngBounds.fromPoints(points),
        padding: EdgeInsets.all(widget.fitPadding),
      ),
    );
  }

  // ---------------------------------------------------------------------
  // 底图切换
  // ---------------------------------------------------------------------

  Future<void> _pickBasemap(BuildContext context) async {
    final state = widget.state;
    final picked = await showModalBottomSheet<String>(
      context: context,
      builder: (ctx) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Padding(
              padding: EdgeInsets.fromLTRB(16, 14, 16, 6),
              child: Align(
                alignment: Alignment.centerLeft,
                child: Text(
                  '底图',
                  style: TextStyle(fontWeight: FontWeight.w600),
                ),
              ),
            ),
            _BasemapTile(
              current: state.basemapOverride == null ? 'auto' : null,
              title: '跟随服务端坐标系',
              subtitle: '当前下发 ${state.config.coordSystem.label}',
              onTap: () => Navigator.of(ctx).pop('auto'),
            ),
            for (final bm in basemaps.values)
              _BasemapTile(
                current:
                    state.basemapOverride == bm.id ? bm.id.wire : null,
                title: bm.label,
                subtitle: '${bm.id.label} · 最大缩放 ${bm.maxZoom}',
                warn: bm.id != state.config.coordSystem,
                onTap: () => Navigator.of(ctx).pop(bm.id.wire),
              ),
            const SizedBox(height: 8),
          ],
        ),
      ),
    );
    if (picked == null) return;
    state.setBasemapOverride(
      picked == 'auto' ? null : CoordSystem.parse(picked),
    );
  }
}

/// 底图选择项。用 `ListTile` 而不是 `RadioListTile`：单选状态自己画，
/// 免得跟 Flutter 各版本里 `groupValue` 的废弃节奏缠在一起。
class _BasemapTile extends StatelessWidget {
  const _BasemapTile({
    required this.current,
    required this.title,
    required this.subtitle,
    required this.onTap,
    this.warn = false,
  });

  /// 选中时是它自己的 key，否则为 null。
  final String? current;
  final String title;
  final String subtitle;
  final VoidCallback onTap;

  /// 与下发坐标系不一致，选了会有偏移。
  final bool warn;

  @override
  Widget build(BuildContext context) {
    final selected = current != null;
    return ListTile(
      leading: Icon(
        selected ? Icons.radio_button_checked : Icons.radio_button_off,
        color: selected ? Theme.of(context).colorScheme.primary : Colors.grey,
      ),
      title: Text(title),
      subtitle: Text(warn ? '$subtitle · 与下发坐标系不一致，会有偏移' : subtitle),
      trailing: warn ? const Icon(Icons.warning_amber, color: Colors.orange) : null,
      onTap: onTap,
    );
  }
}

/// 取 → 送两点之间的圆弧：二次贝塞尔，控制点抬在中点一侧。
///
/// 偏移取弦长的 35%：多单叠在一起时足够分开，单看又不至于像绕路。
List<LatLng> _arcBetween(LatLng a, LatLng b, {int steps = 28}) {
  final midLat = (a.latitude + b.latitude) / 2;
  final midLng = (a.longitude + b.longitude) / 2;
  final dLat = b.latitude - a.latitude;
  final dLng = b.longitude - a.longitude;
  // 法线方向：把弦转 90°，所有订单都朝同一侧弯，不会互相穿插。
  final cLat = midLat - dLng * 0.35;
  final cLng = midLng + dLat * 0.35;

  final points = <LatLng>[];
  for (var i = 0; i <= steps; i++) {
    final t = i / steps;
    final u = 1 - t;
    points.add(
      LatLng(
        u * u * a.latitude + 2 * u * t * cLat + t * t * b.latitude,
        u * u * a.longitude + 2 * u * t * cLng + t * t * b.longitude,
      ),
    );
  }
  return points;
}

/// 取送点的圆标。地图上的字都要有白底，不然压在路网上看不清。
class _PinDot extends StatelessWidget {
  const _PinDot({required this.label, required this.color});

  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) => Container(
        decoration: BoxDecoration(
          color: color,
          shape: BoxShape.circle,
          border: Border.all(color: Colors.white, width: 2),
          boxShadow: const [
            BoxShadow(color: Colors.black26, blurRadius: 3),
          ],
        ),
        alignment: Alignment.center,
        child: Text(
          label,
          style: const TextStyle(
            color: Colors.white,
            fontSize: 11,
            fontWeight: FontWeight.bold,
          ),
        ),
      );
}

class _MapButton extends StatelessWidget {
  const _MapButton({
    required this.icon,
    required this.tooltip,
    required this.onTap,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => Tooltip(
        message: tooltip,
        child: Material(
          color: Colors.white,
          shape: const CircleBorder(),
          elevation: 2,
          child: InkWell(
            customBorder: const CircleBorder(),
            onTap: onTap,
            child: Padding(
              padding: const EdgeInsets.all(9),
              child: Icon(icon, size: 20, color: Colors.black87),
            ),
          ),
        ),
      );
}

/// 底图与下发坐标不匹配的提示条。
class _MismatchBar extends StatelessWidget {
  const _MismatchBar({required this.basemap, required this.coord});

  final Basemap basemap;
  final CoordSystem coord;

  @override
  Widget build(BuildContext context) => Container(
        color: Colors.orange.withValues(alpha: 0.92),
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
        child: Row(
          children: [
            const Icon(Icons.warning_amber, size: 15, color: Colors.white),
            const SizedBox(width: 6),
            Expanded(
              child: Text(
                '底图是 ${basemap.id.label}，点位是 ${coord.label}，会偏几百米。'
                '切成「${basemapFor(coord).label}」就对齐了。',
                style: const TextStyle(fontSize: 11, color: Colors.white),
              ),
            ),
          ],
        ),
      );
}

/// 左下角的版权条。OSM 的使用条款要求署名，高德同理。
class _Attribution extends StatelessWidget {
  const _Attribution({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) => Container(
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
        decoration: BoxDecoration(
          color: Colors.white.withValues(alpha: 0.78),
          borderRadius: BorderRadius.circular(4),
        ),
        child: Text(
          text,
          style: TextStyle(fontSize: 10, color: Colors.grey.shade700),
        ),
      );
}

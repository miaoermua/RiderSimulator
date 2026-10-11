import 'dart:async';

import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';
import '../widgets/order_card.dart';

/// 弹出指派单确认界面：上半地图、下半订单卡。
///
/// 玩法设计 §4.3：控制端强制指派 → 骑手端弹确认框 → 30 秒内不确认自动拒绝。
Future<void> showAssignOrderPage(
  BuildContext context, {
  required GameState state,
  required GameClient client,
}) {
  return Navigator.of(context).push<void>(
    MaterialPageRoute<void>(
      fullscreenDialog: true,
      builder: (_) => AssignOrderPage(state: state, client: client),
    ),
  );
}

class AssignOrderPage extends StatefulWidget {
  const AssignOrderPage({
    super.key,
    required this.state,
    required this.client,
  });

  final GameState state;
  final GameClient client;

  @override
  State<AssignOrderPage> createState() => _AssignOrderPageState();
}

class _AssignOrderPageState extends State<AssignOrderPage> {
  Timer? _ticker;
  bool _resynced = false;

  @override
  void initState() {
    super.initState();
    // 1 秒一次：窗口有 30 秒，走秒已经够用，不必按 100ms 刷。
    _ticker = Timer.periodic(const Duration(seconds: 1), (_) => _beat());
  }

  @override
  void dispose() {
    _ticker?.cancel();
    super.dispose();
  }

  void _beat() {
    if (!mounted) return;
    final state = widget.state;
    if (state.assignOrderId == null) {
      // 已被接受 / 拒绝 / 别人处理掉 → 关界面。
      Navigator.of(context).maybePop();
      return;
    }
    if (state.assignOrder == null && !_resynced) {
      // 单还在路上（order_claimed 不带订单体），补一次快照。
      _resynced = true;
      widget.client.resync();
    }
    setState(() {});
  }

  void _accept() {
    // 服务端已经把这单挂上来了，接受 = 确认，不发上行指令。
    widget.state.resolveAssign();
    Navigator.of(context).maybePop();
  }

  void _reject() {
    final order = widget.state.assignOrder;
    if (order != null) widget.client.reject(order.id);
    widget.state.resolveAssign();
    Navigator.of(context).maybePop();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: widget.state,
      builder: (context, _) {
        final order = widget.state.assignOrder;
        return Scaffold(
          appBar: AppBar(
            title: const Text('平台指派单'),
            leading: IconButton(
              tooltip: '稍后处理',
              onPressed: () => Navigator.of(context).maybePop(),
              icon: const Icon(Icons.close),
            ),
          ),
          body: order == null
              ? const Center(child: Text('正在同步指派单…'))
              : Column(
                  children: [
                    Expanded(
                      flex: 5,
                      child: _AssignMap(order: order),
                    ),
                    Expanded(
                      flex: 4,
                      child: ListView(
                        padding: const EdgeInsets.only(top: 4, bottom: 12),
                        children: [
                          const Padding(
                            padding: EdgeInsets.fromLTRB(14, 10, 14, 0),
                            child: Text(
                              '系统把这一单指派给你，确认后即刻生效。',
                              style: TextStyle(fontSize: 12, color: Colors.grey),
                            ),
                          ),
                          OrderCard(
                            order: order,
                            state: widget.state,
                            client: widget.client,
                            mode: OrderCardMode.assign,
                            onAccept: _accept,
                            onReject: _reject,
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
        );
      },
    );
  }
}

/// 地图区。
///
/// **目前是示意图占位**：真实底图要接高德原生 SDK（`amap_flutter_map`），
/// 服务端已经把坐标按 `coord_system` 转好，接进来是直接可用的。
/// 这里先把「上面地图、下面订单」的版式钉死，免得后面改布局。
class _AssignMap extends StatelessWidget {
  const _AssignMap({required this.order});

  final OrderView order;

  @override
  Widget build(BuildContext context) {
    return Stack(
      children: [
        Positioned.fill(
          child: LayoutBuilder(
            builder: (context, c) {
              final from = Offset(c.maxWidth * 0.24, c.maxHeight * 0.68);
              final to = Offset(c.maxWidth * 0.76, c.maxHeight * 0.30);
              return CustomPaint(
                painter: _MapPainter(from: from, to: to),
                child: const SizedBox.expand(),
              );
            },
          ),
        ),
        Positioned(
          left: 12,
          top: 12,
          child: _MapChip(
            icon: Icons.straighten,
            text: '全程 ${order.distanceLabel}',
          ),
        ),
        Positioned(
          right: 12,
          top: 12,
          child: _MapChip(
            icon: Icons.schedule,
            text: '限 ${order.promisedMinutes} 分钟',
          ),
        ),
        const Positioned(
          left: 12,
          bottom: 12,
          child: _MapChip(
            icon: Icons.map_outlined,
            text: '地图占位 · 待接高德',
          ),
        ),
      ],
    );
  }
}

class _MapChip extends StatelessWidget {
  const _MapChip({required this.icon, required this.text});

  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
      decoration: BoxDecoration(
        color: Colors.white.withValues(alpha: 0.92),
        borderRadius: BorderRadius.circular(14),
        boxShadow: const [
          BoxShadow(color: Colors.black12, blurRadius: 6, offset: Offset(0, 2)),
        ],
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 14, color: Colors.grey.shade700),
          const SizedBox(width: 4),
          Text(text, style: const TextStyle(fontSize: 12)),
        ],
      ),
    );
  }
}

/// 画底图网格 + 取送虚线 + 两端坐标点。
class _MapPainter extends CustomPainter {
  _MapPainter({required this.from, required this.to});

  final Offset from;
  final Offset to;

  static const _grid = Color(0xFFD6E2D8);
  static const _route = Color(0xFF1976D2);

  @override
  void paint(Canvas canvas, Size size) {
    final bg = Paint()..color = const Color(0xFFEAF1EA);
    canvas.drawRect(Offset.zero & size, bg);

    final grid = Paint()
      ..color = _grid
      ..strokeWidth = 1;
    const step = 28.0;
    for (double x = 0; x < size.width; x += step) {
      canvas.drawLine(Offset(x, 0), Offset(x, size.height), grid);
    }
    for (double y = 0; y < size.height; y += step) {
      canvas.drawLine(Offset(0, y), Offset(size.width, y), grid);
    }

    final path = Path()
      ..moveTo(from.dx, from.dy)
      ..quadraticBezierTo(
        (from.dx + to.dx) / 2,
        from.dy - size.height * 0.35,
        to.dx,
        to.dy,
      );
    final route = Paint()
      ..color = _route
      ..strokeWidth = 4
      ..strokeCap = StrokeCap.round
      ..style = PaintingStyle.stroke;
    canvas.drawPath(_dashed(path), route);

    for (final p in [from, to]) {
      canvas.drawCircle(p, 9, Paint()..color = Colors.white);
      canvas.drawCircle(p, 6, Paint()..color = _route);
    }
  }

  /// 把实线路径切成一段段虚线。
  Path _dashed(Path source) {
    const dash = 8.0;
    const gap = 6.0;
    final out = Path();
    for (final metric in source.computeMetrics()) {
      double d = 0;
      while (d < metric.length) {
        final end = (d + dash).clamp(0.0, metric.length);
        out.addPath(metric.extractPath(d, end), Offset.zero);
        d = end + gap;
      }
    }
    return out;
  }

  @override
  bool shouldRepaint(_MapPainter old) => old.from != from || old.to != to;
}

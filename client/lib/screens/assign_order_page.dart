import 'dart:async';

import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';
import '../widgets/delivery_map.dart';
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
                      child: _AssignMap(order: order, state: widget.state),
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

/// 地图区：真底图 +「取 → 送」连线，顶部压一条距离/时限。
///
/// 底图跟着服务端下发的坐标系走（GCJ-02 → 高德，WGS-84 → OSM），
/// 也能在图上的图层按钮里手动换；换得与坐标系不一致时会挂提示条。
class _AssignMap extends StatelessWidget {
  const _AssignMap({required this.order, required this.state});

  final OrderView order;
  final GameState state;

  @override
  Widget build(BuildContext context) {
    return Stack(
      children: [
        Positioned.fill(
          child: DeliveryMap(
            state: state,
            orders: [order],
            // 指派单先回答「这一单从哪到哪、我现在在哪」，
            // 三十多个商户点只会把画面糊住。
            showMerchants: false,
            showMe: true,
          ),
        ),
        Positioned(
          left: 12,
          // 让开底图与坐标系不一致时的橙色提示条。
          top: 38,
          child: _MapChip(
            icon: Icons.straighten,
            text: '全程 ${order.distanceLabel}',
          ),
        ),
        Positioned(
          right: 12,
          top: 38,
          child: _MapChip(
            icon: Icons.schedule,
            text: '限 ${order.promisedMinutes} 分钟',
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

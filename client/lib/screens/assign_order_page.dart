import 'dart:async';

import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';
import '../widgets/app_toast.dart';
import '../widgets/delivery_map.dart';
import '../widgets/order_card.dart';

/// 弹出派单确认界面：上半地图、下半订单卡。
///
/// 玩法设计 §4.3：控制端派单 → 骑手端弹确认框 → 30 秒内不处理自动收尾。
/// 两种口径共用这一页：
/// - 指派单（`offer`）：可以拒绝，拒一次吃一格当日额度，30 秒不处理自动拒；
/// - 强制派单（`forced`）：只能同意，30 秒不处理自动接下。
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
    if (state.assignRemainingMs <= 0) {
      // 30 秒到点。强制派单没有拒绝这个动作，只能是「自动接下」；
      // 指派单额度用尽时「自动拒绝」也发不出去，同样只能收下——
      // 不然骑手会卡在一张既拒不掉、又没确认的单上。
      if (state.assignForced || state.rejectQuotaExhausted) {
        _accept(auto: true);
      } else {
        _reject(auto: true);
      }
      return;
    }
    if (state.assignOrder == null && !_resynced) {
      // 单还在路上（order_claimed 不带订单体），补一次快照。
      _resynced = true;
      widget.client.resync();
    }
    setState(() {});
  }

  /// 接受。**必须发上行**：服务端靠 `assign_pending` 区分「拒掉一张没确认的
  /// 指派单」（吃额度）和「取消一张已经接下的单」（罚钱）。不确认就退单，
  /// 会被当成前者，白吃一次额度。
  void _accept({bool auto = false}) {
    final state = widget.state;
    final id = state.assignOrder?.id ?? state.assignOrderId;
    if (id != null) widget.client.acceptAssign(id);
    if (auto) {
      state.note(
        state.assignForced ? '30 秒未确认，强制派单已自动接下' : '30 秒未确认，这单已自动接下',
        kind: LogKind.info,
      );
    }
    state.resolveAssign();
    Navigator.of(context).maybePop();
  }

  void _reject({bool auto = false}) {
    final state = widget.state;
    if (state.assignForced) {
      // 按钮已经藏了，这里兜住误触 / 旧状态：说清楚为什么拒不了。
      state.note('强制派单不能拒绝', kind: LogKind.bad);
      showAppToast('强制派单不能拒绝', bad: true);
      return;
    }
    if (state.rejectQuotaExhausted) {
      // 服务端一定会挡回来，就地提示比让骑手以为拒成功了强。
      state.note('你今天拒绝已超出上限', kind: LogKind.bad);
      showAppToast('你今天拒绝已超出上限', bad: true);
      return;
    }
    final order = state.assignOrder;
    if (order != null) widget.client.reject(order.id);
    if (auto) state.note('30 秒未确认，已自动拒单', kind: LogKind.info);
    state.resolveAssign();
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
            title: Text(widget.state.assignForced ? '强制派单' : '指派单'),
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
                          Padding(
                            padding: const EdgeInsets.fromLTRB(14, 10, 14, 0),
                            child: Text(
                              widget.state.assignForced
                                  ? '这是强制派单，只能同意；30 秒不处理会自动接下。'
                                  : '这是指派单，可以拒绝（消耗一次当日额度）；30 秒不处理会自动拒单。',
                              style: const TextStyle(
                                  fontSize: 12, color: Colors.grey),
                            ),
                          ),
                          // 强制派单没有拒绝权，额度条对他没有意义。
                          if (!widget.state.assignForced)
                            _QuotaBanner(state: widget.state),
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

/// 拒单额度条。
///
/// 额度是当天的一次性资源，骑手拒绝之前必须知道还剩几次——拒超了就是
/// 「这一单只能收下」，是本页最该说清楚的一件事。
class _QuotaBanner extends StatelessWidget {
  const _QuotaBanner({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    final left = state.rejectQuotaLeft;
    final used = state.me?.rejectsToday ?? 0;
    final limit = state.config.maxRejectsPerDay;

    final (text, fg, bg, icon) = state.rejectQuotaExhausted
        ? ('你今天拒绝已超出上限，这一单拒不掉、只能收下', Colors.red.shade800, Colors.red.shade50, Icons.block)
        : left == null
            ? ('今日拒单不限次数（已拒 $used 单）', Colors.grey.shade800, Colors.grey.shade100, Icons.info_outline)
            : ('今日还能拒 $left 单（已用 $used/$limit）', Colors.orange.shade900, Colors.orange.shade50, Icons.info_outline);

    return Container(
      width: double.infinity,
      margin: const EdgeInsets.fromLTRB(12, 8, 12, 0),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(color: bg, borderRadius: BorderRadius.circular(8)),
      child: Row(
        children: [
          Icon(icon, size: 16, color: fg),
          const SizedBox(width: 6),
          Expanded(
            child: Text(text, style: TextStyle(fontSize: 12, color: fg)),
          ),
        ],
      ),
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

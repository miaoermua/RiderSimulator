import 'package:flutter/material.dart';
import 'package:latlong2/latlong.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';
import '../screens/order_detail_page.dart';
import 'chat_sheet.dart';
import 'delivery_map.dart';
import 'slide_confirm.dart';

/// 订单卡的形态：决定底部操作区画什么。
enum OrderCardMode {
  /// 大厅待抢：抢单按钮 + 抢单倒计时条。
  hall,

  /// 待取货：到店 / 催单。
  pickup,

  /// 待送达：送达 / 催单。
  delivery,

  /// 指派单确认：拒绝 / 接受。
  assign,
}

/// 订单卡片。
///
/// 四个形态共用同一套信息区（报酬、距离、时限、备注、地址），
/// 只有底部操作区不同——这是众包骑手端订单卡的通行做法：
/// 卡片是决策信息面板，按钮才是状态机。
class OrderCard extends StatefulWidget {
  const OrderCard({
    super.key,
    required this.order,
    required this.state,
    required this.client,
    this.mode = OrderCardMode.hall,
    this.onAccept,
    this.onReject,
    this.showRouteRail = true,
    this.showActions = true,
  });

  final OrderView order;
  final GameState state;
  final GameClient client;
  final OrderCardMode mode;

  /// 仅 [OrderCardMode.assign] 用。
  final VoidCallback? onAccept;
  final VoidCallback? onReject;

  /// 左侧的「取 / 送」步进条。订单详情页已经有大图，不必再来一个。
  final bool showRouteRail;

  /// 底部操作区。已结束的订单在详情页里只看不操作。
  final bool showActions;

  @override
  State<OrderCard> createState() => _OrderCardState();
}

class _OrderCardState extends State<OrderCard> {
  /// 当前聚焦的点：0 = 取（商家），1 = 送（顾客）。
  int _focus = 0;

  /// 卡片上方展开的地图。
  bool _mapOpen = false;

  @override
  Widget build(BuildContext context) {
    final order = widget.order;
    final state = widget.state;
    final mode = widget.mode;
    final theme = Theme.of(context);
    final expired =
        mode == OrderCardMode.hall && state.claimRemainingMs(order) <= 0;

    return Card(
      margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      clipBehavior: Clip.antiAlias,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _mapPane(),
          IntrinsicHeight(
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                if (widget.showRouteRail) _rail(context),
                Expanded(
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(14, 12, 14, 8),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Row(
                          children: [
                            Expanded(
                              child: Text(
                                order.merchantName,
                                style: theme.textTheme.titleMedium
                                    ?.copyWith(fontWeight: FontWeight.w600),
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                            ),
                            if (mode == OrderCardMode.assign ||
                                order.forced) ...[
                              const _Chip(
                                  text: '平台指派', color: Colors.redAccent),
                              const SizedBox(width: 6),
                            ],
                            _TierChip(tier: order.tier),
                            if (order.backOrder) ...[
                              const SizedBox(width: 6),
                              const _Chip(text: '背单', color: Colors.teal),
                            ],
                          ],
                        ),
                        const SizedBox(height: 6),
                        Row(
                          children: [
                            _Stat(icon: Icons.route, text: order.distanceLabel),
                            const SizedBox(width: 14),
                            _Stat(
                                icon: Icons.schedule,
                                text: '${order.promisedMinutes} 分钟'),
                            const SizedBox(width: 14),
                            _Stat(
                              icon: Icons.local_shipping_outlined,
                              text: order.status.label,
                            ),
                          ],
                        ),
                        if (order.remark.isNotEmpty) ...[
                          const SizedBox(height: 6),
                          Text(
                            '备注：${order.remark}',
                            style: theme.textTheme.bodySmall
                                ?.copyWith(color: Colors.orange),
                            maxLines: 2,
                            overflow: TextOverflow.ellipsis,
                          ),
                        ],
                        const SizedBox(height: 6),
                        _Leg(
                          icon: Icons.location_on_outlined,
                          label: '送',
                          text: order.customerAddress.isEmpty
                              ? order.customerName
                              : '${order.customerName} · ${order.customerAddress}',
                        ),
                        const Divider(height: 20),
                        Text(
                          '到手 ¥${yuan(order.effectiveTakehomeCents)}',
                          style: theme.textTheme.titleMedium?.copyWith(
                            color: Colors.green.shade700,
                            fontWeight: FontWeight.bold,
                          ),
                        ),
                        if (order.fee.rainBonusCents > 0)
                          Text(
                            '含雨补 ¥${yuan(order.fee.rainBonusCents)}',
                            style: theme.textTheme.labelSmall,
                          ),
                        // 操作区单独一行靠右：左侧多了 58 宽的步进条之后，
                        // 「报酬 + 两个按钮」并排在 360 宽的小屏上会溢出。
                        if (widget.showActions &&
                            mode != OrderCardMode.hall) ...[
                          const SizedBox(height: 6),
                          Align(
                            alignment: Alignment.centerRight,
                            child: _Actions(
                              order: order,
                              state: state,
                              client: widget.client,
                              mode: mode,
                              expired: expired,
                              onAccept: widget.onAccept,
                              onReject: widget.onReject,
                            ),
                          ),
                        ],
                        if (mode == OrderCardMode.hall)
                          _CountdownBar(
                              order: order, state: state, expired: expired),
                        if (widget.showActions &&
                            mode == OrderCardMode.hall &&
                            state.isRider) ...[
                          const SizedBox(height: 8),
                          SizedBox(
                            width: double.infinity,
                            height: 44,
                            child: SlideToConfirm(
                              label: expired ? '已截止，无法抢单' : '滑动抢单',
                              icon: Icons.pan_tool_alt,
                              enabled: !expired,
                              onConfirm: () => widget.client.claim(order.id),
                            ),
                          ),
                        ],
                        if (widget.showActions &&
                            mode == OrderCardMode.delivery) ...[
                          const SizedBox(height: 8),
                          SizedBox(
                            width: double.infinity,
                            height: 44,
                            child: SlideToConfirm(
                              label: '滑动确认送达',
                              icon: Icons.done_all,
                              color: Colors.green.shade600,
                              onConfirm: () => widget.client.deliver(order.id),
                            ),
                          ),
                        ],
                        if (mode == OrderCardMode.assign)
                          _AssignBar(state: state),
                      ],
                    ),
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  // ---------------------------------------------------------------------
  // 路线：左侧步进条 + 上方展开的地图
  // ---------------------------------------------------------------------

  /// 当前聚焦的点。
  LatLng get _focusPoint {
    final o = widget.order;
    return _focus == 0
        ? LatLng(o.merchantLat, o.merchantLng)
        : LatLng(o.customerLat, o.customerLng);
  }

  /// 中间那行距离：优先「我 → 当前点」的直线距离（骑手最关心的那个数），
  /// 没定位时退回订单本身的取送里程。
  String get _distanceLabel {
    final o = widget.order;
    final me = widget.state.me;
    if (me == null) return o.distanceLabel;
    final m = const Distance()
        .as(LengthUnit.Meter, LatLng(me.lat, me.lng), _focusPoint);
    return m >= 1000 ? '${(m / 1000).toStringAsFixed(1)}km' : '${m.round()}m';
  }

  /// 左侧竖条：上箭头（上一个点）→ 距离 → 下箭头（下一个点）。
  ///
  /// 点中间展开地图，点箭头只切点；地图开着时会跟着平移过去。
  Widget _rail(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final atFirst = _focus == 0;
    return Container(
      width: 58,
      decoration: BoxDecoration(
        color: scheme.primary.withValues(alpha: 0.05),
        border: const Border(right: BorderSide(color: Colors.black12)),
      ),
      child: Column(
        children: [
          _railArrow(
            icon: Icons.keyboard_arrow_up,
            tip: '上一个点',
            enabled: !atFirst,
            onTap: () => setState(() => _focus = 0),
          ),
          Expanded(
            child: InkWell(
              onTap: () => setState(() => _mapOpen = !_mapOpen),
              child: Center(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      atFirst ? '取' : '送',
                      style: TextStyle(
                        fontSize: 11,
                        color: Colors.grey.shade700,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    Text(
                      _distanceLabel,
                      style: const TextStyle(
                        fontSize: 12,
                        fontWeight: FontWeight.w700,
                      ),
                    ),
                    const SizedBox(height: 2),
                    Icon(
                      _mapOpen ? Icons.map : Icons.map_outlined,
                      size: 14,
                      color: scheme.primary,
                    ),
                  ],
                ),
              ),
            ),
          ),
          _railArrow(
            icon: Icons.keyboard_arrow_down,
            tip: '下一个点',
            enabled: atFirst,
            onTap: () => setState(() => _focus = 1),
          ),
        ],
      ),
    );
  }

  Widget _railArrow({
    required IconData icon,
    required String tip,
    required bool enabled,
    required VoidCallback onTap,
  }) =>
      SizedBox(
        height: 24,
        child: Tooltip(
          message: tip,
          child: InkWell(
            onTap: enabled ? onTap : null,
            child: Icon(
              icon,
              size: 18,
              color: enabled ? Colors.black54 : Colors.black12,
            ),
          ),
        ),
      );

  /// 订单卡上沿伸出的地图。再点一下地图进订单详情。
  Widget _mapPane() => AnimatedSize(
        duration: const Duration(milliseconds: 180),
        curve: Curves.easeOut,
        alignment: Alignment.topCenter,
        child: !_mapOpen
            ? const SizedBox(width: double.infinity)
            : SizedBox(
                height: 170,
                child: Stack(
                  children: [
                    Positioned.fill(
                      child: DeliveryMap(
                        state: widget.state,
                        orders: [widget.order],
                        showMerchants: false,
                        showMe: true,
                        showBasemapPicker: false,
                        focus: _focusPoint,
                      ),
                    ),
                    // 只吃「点一下」，拖动 / 缩放仍然归地图。
                    Positioned.fill(
                      child: Material(
                        color: Colors.transparent,
                        child: InkWell(onTap: _openDetail),
                      ),
                    ),
                    Positioned(
                      right: 8,
                      bottom: 8,
                      child: IgnorePointer(
                        child: Container(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 8,
                            vertical: 4,
                          ),
                          decoration: BoxDecoration(
                            color: Colors.white.withValues(alpha: 0.92),
                            borderRadius: BorderRadius.circular(12),
                          ),
                          child: const Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              Text('订单详情', style: TextStyle(fontSize: 11)),
                              Icon(Icons.chevron_right, size: 14),
                            ],
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
      );

  void _openDetail() => showOrderDetail(
        context,
        order: widget.order,
        state: widget.state,
        client: widget.client,
      );
}

class _Actions extends StatelessWidget {
  const _Actions({
    required this.order,
    required this.state,
    required this.client,
    required this.mode,
    required this.expired,
    required this.onAccept,
    required this.onReject,
  });

  final OrderView order;
  final GameState state;
  final GameClient client;
  final OrderCardMode mode;
  final bool expired;
  final VoidCallback? onAccept;
  final VoidCallback? onReject;

  /// 「在线对话」按钮上的字。任务对话也在这条时间线里，所以一并算「新消息」。
  ///
  /// 顾客开口是稀罕事（大约 5% 的单子），一来消息就值得在按钮上报一声，
  /// 免得骑手错过——尤其是带赔付的那种。
  String get _chatLabel {
    final has = state.timelineFor(order.id).any((e) => e.from == 'customer');
    return has ? '在线对话 · 新消息' : '在线对话';
  }

  void _openChat(BuildContext context) => showChatSheet(
        context,
        order: order,
        state: state,
        client: client,
      );

  @override
  Widget build(BuildContext context) {
    switch (mode) {
      case OrderCardMode.hall:
        if (!state.isRider) {
          return const Text('观战', style: TextStyle(color: Colors.grey));
        }
        // 抢单是整条滑动确认，不在这里占位（见 _OrderCardState.build）。
        return const SizedBox.shrink();

      case OrderCardMode.pickup:
        return Wrap(
          spacing: 8,
          children: [
            // 末端交付沟通就放在原「催单」的位置：取货路上最常做的事是
            // 跟顾客说一声「到楼下了」，而不是催商家。
            // 催单挪去了订单详情页（见 OrderDetailPage）。
            OutlinedButton.icon(
              onPressed: () => _openChat(context),
              icon: const Icon(Icons.support_agent, size: 18),
              label: Text(_chatLabel),
            ),
            FilledButton(
              onPressed: () => client.arriveShop(order.id),
              child: const Text('到店'),
            ),
          ],
        );

      case OrderCardMode.delivery:
        // 「送达」是整条滑动确认，这里只留联系顾客。
        return OutlinedButton.icon(
          onPressed: () => _openChat(context),
          icon: const Icon(Icons.support_agent, size: 18),
          label: Text(_chatLabel),
        );

      case OrderCardMode.assign:
        // 额度用尽时这一单本来就不许拒。按钮挂着「拒不掉」而不是置灰：
        // 点下去会弹中心通知把原因说清楚（见 AssignOrderPage._reject），
        // 置灰的按钮只能让人猜。
        final noQuota = state.rejectQuotaExhausted;
        return Wrap(
          spacing: 8,
          children: [
            OutlinedButton(
              onPressed: onReject,
              child: Text(noQuota ? '拒不掉' : '拒绝'),
            ),
            FilledButton(
              onPressed: onAccept,
              child: const Text('接受'),
            ),
          ],
        );
    }
  }
}

/// 一行「标签 + 内容」，用来放配送地址。
class _Leg extends StatelessWidget {
  const _Leg({required this.icon, required this.label, required this.text});

  final IconData icon;
  final String label;
  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Icon(icon, size: 15, color: Colors.grey.shade600),
        const SizedBox(width: 4),
        Text(
          label,
          style: theme.textTheme.labelSmall?.copyWith(color: Colors.grey),
        ),
        const SizedBox(width: 6),
        Expanded(
          child: Text(
            text,
            style: theme.textTheme.bodySmall,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
          ),
        ),
      ],
    );
  }
}

/// 大厅倒计时条：越接近截止越红。
class _CountdownBar extends StatelessWidget {
  const _CountdownBar({
    required this.order,
    required this.state,
    required this.expired,
  });

  final OrderView order;
  final GameState state;
  final bool expired;

  @override
  Widget build(BuildContext context) {
    final total =
        (order.claimDeadlineMin - order.createdAtMin).clamp(1, 1 << 30);
    final remainMs = state.claimRemainingMs(order);
    final remainSec = ((remainMs + 999) ~/ 1000).clamp(0, 1 << 30);
    final frac = ((remainMs / 1000) / total).clamp(0.0, 1.0);
    final color = expired
        ? Colors.grey
        : remainSec < 2
            ? Colors.red
            : remainSec < 4
                ? Colors.orange
                : Colors.green;

    return _Bar(
      frac: frac,
      color: color,
      trailing: expired ? '已截止' : '${remainSec}s',
    );
  }
}

/// 指派单确认倒计时条。玩法设计 §4.3：30 秒内不确认则自动拒绝。
class _AssignBar extends StatelessWidget {
  const _AssignBar({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    final ms = state.assignRemainingMs;
    final sec = ((ms + 999) ~/ 1000).clamp(0, 1 << 30);
    final frac = (ms / GameState.assignWindow.inMilliseconds).clamp(0.0, 1.0);
    final exhausted = state.rejectQuotaExhausted;
    // 30 秒的窗口，最后 5 秒才转红。额度用尽时整条也不该再是「橙色的催促」，
    // 因为到点之后的归宿已经变了：不是自动拒单，是自动收下。
    final color =
        exhausted ? Colors.red : (sec <= 5 ? Colors.red : Colors.deepOrange);

    return _Bar(
      frac: frac,
      color: color,
      trailing: exhausted
          ? '拒单额度已用尽，到点自动收下'
          : (sec == 0 ? '已超时，尽快处理' : '${sec}s 内确认'),
    );
  }
}

class _Bar extends StatelessWidget {
  const _Bar({
    required this.frac,
    required this.color,
    required this.trailing,
  });

  final double frac;
  final Color color;
  final String trailing;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(top: 8),
      child: Row(
        children: [
          Expanded(
            child: ClipRRect(
              borderRadius: BorderRadius.circular(4),
              child: LinearProgressIndicator(
                value: frac,
                minHeight: 5,
                backgroundColor: Colors.black12,
                valueColor: AlwaysStoppedAnimation(color),
              ),
            ),
          ),
          const SizedBox(width: 8),
          Text(
            trailing,
            style: TextStyle(color: color, fontWeight: FontWeight.w600),
          ),
        ],
      ),
    );
  }
}

class _TierChip extends StatelessWidget {
  const _TierChip({required this.tier});

  final OrderTier tier;

  @override
  Widget build(BuildContext context) {
    final color = switch (tier) {
      OrderTier.normal => Colors.blueGrey,
      OrderTier.premium => Colors.deepPurple,
      OrderTier.express => Colors.redAccent,
    };
    return _Chip(text: tier.label, color: color);
  }
}

class _Chip extends StatelessWidget {
  const _Chip({required this.text, required this.color});

  final String text;
  final Color color;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: color.withValues(alpha: 0.5)),
      ),
      child: Text(
        text,
        style:
            TextStyle(color: color, fontSize: 12, fontWeight: FontWeight.w600),
      ),
    );
  }
}

class _Stat extends StatelessWidget {
  const _Stat({required this.icon, required this.text});

  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(icon, size: 15, color: Colors.grey.shade600),
        const SizedBox(width: 4),
        Text(text, style: Theme.of(context).textTheme.bodySmall),
      ],
    );
  }
}

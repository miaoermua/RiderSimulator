import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';

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
class OrderCard extends StatelessWidget {
  const OrderCard({
    super.key,
    required this.order,
    required this.state,
    required this.client,
    this.mode = OrderCardMode.hall,
    this.onAccept,
    this.onReject,
  });

  final OrderView order;
  final GameState state;
  final GameClient client;
  final OrderCardMode mode;

  /// 仅 [OrderCardMode.assign] 用。
  final VoidCallback? onAccept;
  final VoidCallback? onReject;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final expired =
        mode == OrderCardMode.hall && state.claimRemainingMs(order) <= 0;

    return Card(
      margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      clipBehavior: Clip.antiAlias,
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
                if (mode == OrderCardMode.assign || order.forced) ...[
                  const _Chip(text: '平台指派', color: Colors.redAccent),
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
                _Stat(icon: Icons.schedule, text: '${order.promisedMinutes} 分钟'),
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
                style: theme.textTheme.bodySmall?.copyWith(color: Colors.orange),
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
            Row(
              crossAxisAlignment: CrossAxisAlignment.end,
              children: [
                Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
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
                  ],
                ),
                const Spacer(),
                _Actions(
                  order: order,
                  state: state,
                  client: client,
                  mode: mode,
                  expired: expired,
                  onAccept: onAccept,
                  onReject: onReject,
                ),
              ],
            ),
            if (mode == OrderCardMode.hall)
              _CountdownBar(order: order, state: state, expired: expired),
            if (mode == OrderCardMode.assign)
              _AssignBar(state: state),
          ],
        ),
      ),
    );
  }
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

  @override
  Widget build(BuildContext context) {
    switch (mode) {
      case OrderCardMode.hall:
        if (!state.isRider) {
          return const Text('观战', style: TextStyle(color: Colors.grey));
        }
        return FilledButton.icon(
          onPressed: expired ? null : () => client.claim(order.id),
          icon: const Icon(Icons.pan_tool_alt, size: 18),
          label: const Text('抢单'),
        );

      case OrderCardMode.pickup:
        return Wrap(
          spacing: 8,
          children: [
            OutlinedButton(
              onPressed: () => client.urge(order.id),
              child: const Text('催单'),
            ),
            FilledButton(
              onPressed: () => client.arriveShop(order.id),
              child: const Text('到店'),
            ),
          ],
        );

      case OrderCardMode.delivery:
        return Wrap(
          spacing: 8,
          children: [
            OutlinedButton(
              onPressed: () => client.urge(order.id),
              child: const Text('催单'),
            ),
            FilledButton.icon(
              onPressed: () => client.deliver(order.id),
              icon: const Icon(Icons.done_all, size: 18),
              label: const Text('送达'),
            ),
          ],
        );

      case OrderCardMode.assign:
        return Wrap(
          spacing: 8,
          children: [
            OutlinedButton(
              onPressed: onReject,
              child: const Text('拒绝'),
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
    final total = (order.claimDeadlineMin - order.createdAtMin).clamp(1, 1 << 30);
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
    // 30 秒的窗口，最后 5 秒才转红。
    final color = sec <= 5 ? Colors.red : Colors.deepOrange;

    return _Bar(
      frac: frac,
      color: color,
      trailing: sec == 0 ? '已超时，尽快处理' : '${sec}s 内确认',
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
        style: TextStyle(color: color, fontSize: 12, fontWeight: FontWeight.w600),
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

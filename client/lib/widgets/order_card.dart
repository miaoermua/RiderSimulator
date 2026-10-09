import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';

/// 订单卡片。大厅里显示「抢单」，我的单里显示阶段按钮。
class OrderCard extends StatelessWidget {
  const OrderCard({
    super.key,
    required this.order,
    required this.state,
    required this.client,
    this.mine = false,
  });

  final OrderView order;
  final GameState state;
  final GameClient client;
  final bool mine;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final expired = !mine && state.claimRemainingMs(order) <= 0;

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
            const SizedBox(height: 4),
            Text(
              '送：${order.customerAddress.isEmpty ? order.customerName : order.customerAddress}',
              style: theme.textTheme.bodySmall,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
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
                if (!mine)
                  _HallActions(
                    order: order,
                    state: state,
                    client: client,
                    expired: expired,
                  )
                else
                  _MineActions(order: order, client: client),
              ],
            ),
            if (!mine) _CountdownBar(order: order, state: state, expired: expired),
          ],
        ),
      ),
    );
  }
}

class _HallActions extends StatelessWidget {
  const _HallActions({
    required this.order,
    required this.state,
    required this.client,
    required this.expired,
  });

  final OrderView order;
  final GameState state;
  final GameClient client;
  final bool expired;

  @override
  Widget build(BuildContext context) {
    if (!state.isRider) {
      return const Text('观战', style: TextStyle(color: Colors.grey));
    }
    return FilledButton.icon(
      onPressed: expired ? null : () => client.claim(order.id),
      icon: const Icon(Icons.pan_tool_alt, size: 18),
      label: const Text('抢单'),
    );
  }
}

class _MineActions extends StatelessWidget {
  const _MineActions({
    required this.order,
    required this.client,
  });

  final OrderView order;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    return switch (order.status) {
      OrderStatus.assigned => Wrap(
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
        ),
      OrderStatus.pickedUp => Wrap(
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
        ),
      _ => const SizedBox.shrink(),
    };
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
    final remainSec = (remainMs / 1000).clamp(0, 1 << 30);
    final frac = ((remainMs / 1000) / total).clamp(0.0, 1.0);
    final color = expired
        ? Colors.grey
        : remainSec < 2
            ? Colors.red
            : remainSec < 4
                ? Colors.orange
                : Colors.green;

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
            expired ? '已截止' : '${remainSec}s',
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
        color: color.withOpacity(0.12),
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: color.withOpacity(0.5)),
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

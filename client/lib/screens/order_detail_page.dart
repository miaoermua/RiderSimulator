import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';
import '../widgets/delivery_map.dart';
import '../widgets/order_card.dart';

/// 打开某一单的详情页。订单卡上点展开的地图就走这里。
Future<void> showOrderDetail(
  BuildContext context, {
  required OrderView order,
  required GameState state,
  required GameClient client,
}) {
  return Navigator.of(context).push<void>(
    MaterialPageRoute<void>(
      builder: (_) => OrderDetailPage(
        orderId: order.id,
        state: state,
        client: client,
      ),
    ),
  );
}

/// 订单详情：大图 + 卡片 + 时间线。
///
/// 只认 id，不持有 `OrderView`——详情页开着的时候订单可能被抢走 / 送达 /
/// 取消，每次都从 [GameState] 里取最新的一份，界面才不会拿旧数据骗人。
class OrderDetailPage extends StatelessWidget {
  const OrderDetailPage({
    super.key,
    required this.orderId,
    required this.state,
    required this.client,
  });

  final int orderId;
  final GameState state;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: state,
      builder: (context, _) {
        final order = state.orderById(orderId);
        return Scaffold(
          appBar: AppBar(
            title: Text(order == null ? '订单详情' : '订单 #${order.id}'),
            actions: [
              if (order != null) _StatusChip(status: order.status),
              const SizedBox(width: 8),
            ],
          ),
          body: order == null
              ? const Center(
                  child: Text(
                    '这一单已经不在大厅和你的手上。',
                    style: TextStyle(color: Colors.grey),
                  ),
                )
              : ListView(
                  padding: const EdgeInsets.only(bottom: 24),
                  children: [
                    SizedBox(
                      height: 260,
                      child: DeliveryMap(
                        state: state,
                        orders: [order],
                        showMerchants: false,
                        showMe: true,
                      ),
                    ),
                    OrderCard(
                      order: order,
                      state: state,
                      client: client,
                      mode: _modeFor(order),
                      // 上面已经是大图，卡片里不用再展开一个。
                      showRouteRail: false,
                      // 已结束的单只看不操作。
                      showActions: order.status.isActive,
                    ),
                    // 催单挪进详情页：它只对「已到店、还没出餐」有用
                    // （服务端要求 Phase::WaitingFood），放在列表卡上
                    // 十有八九点下去是被拒，不如收进详情里当低频操作。
                    if (order.status == OrderStatus.assigned)
                      Padding(
                        padding: const EdgeInsets.fromLTRB(16, 4, 16, 0),
                        child: Row(
                          children: [
                            OutlinedButton.icon(
                              onPressed: () => client.urge(order.id),
                              icon: const Icon(Icons.campaign_outlined, size: 18),
                              label: const Text('催单'),
                            ),
                            const SizedBox(width: 10),
                            Text(
                              order.urgeCount == 0
                                  ? '催商家快点出餐'
                                  : '已催 ${order.urgeCount} 次',
                              style: const TextStyle(
                                fontSize: 12,
                                color: Colors.grey,
                              ),
                            ),
                          ],
                        ),
                      ),
                    _Timeline(order: order),
                  ],
                ),
        );
      },
    );
  }

  static OrderCardMode _modeFor(OrderView o) => switch (o.status) {
        OrderStatus.pending => OrderCardMode.hall,
        OrderStatus.assigned => OrderCardMode.pickup,
        OrderStatus.pickedUp => OrderCardMode.delivery,
        // 已送达 / 取消 / 过期：操作区已经关掉，形态无所谓。
        _ => OrderCardMode.hall,
      };
}

class _StatusChip extends StatelessWidget {
  const _StatusChip({required this.status});

  final OrderStatus status;

  @override
  Widget build(BuildContext context) {
    final color = switch (status) {
      OrderStatus.pending => Colors.blue,
      OrderStatus.assigned => Colors.orange,
      OrderStatus.pickedUp => Colors.green,
      OrderStatus.delivered => Colors.green.shade700,
      OrderStatus.late => Colors.deepOrange,
      OrderStatus.expired || OrderStatus.cancelled => Colors.grey,
    };
    return Center(
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 3),
        decoration: BoxDecoration(
          color: color.withValues(alpha: 0.12),
          borderRadius: BorderRadius.circular(10),
          border: Border.all(color: color.withValues(alpha: 0.5)),
        ),
        child: Text(
          status.label,
          style: TextStyle(
            color: color,
            fontSize: 12,
            fontWeight: FontWeight.w600,
          ),
        ),
      ),
    );
  }
}

/// 时间线：下单 / 取货 / 送达。没有的时刻不画，免得出现一排「--:--」。
class _Timeline extends StatelessWidget {
  const _Timeline({required this.order});

  final OrderView order;

  @override
  Widget build(BuildContext context) {
    final rows = <(String, String)>[
      ('下单', clockLabel(order.createdAtMin)),
      if (order.pickedUpAtMin != null) ('取货', clockLabel(order.pickedUpAtMin!)),
      if (order.deliveredAtMin != null)
        ('送达', clockLabel(order.deliveredAtMin!)),
      ('承诺时限', '${order.promisedMinutes} 分钟'),
    ];
    return Padding(
      padding: const EdgeInsets.fromLTRB(20, 4, 20, 0),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const SizedBox(height: 4),
          Text(
            '时间',
            style: Theme.of(context)
                .textTheme
                .labelMedium
                ?.copyWith(color: Colors.grey),
          ),
          const SizedBox(height: 6),
          for (final (label, value) in rows)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 3),
              child: Row(
                children: [
                  SizedBox(
                    width: 68,
                    child: Text(
                      label,
                      style: const TextStyle(fontSize: 12, color: Colors.grey),
                    ),
                  ),
                  Text(value, style: const TextStyle(fontSize: 12)),
                ],
              ),
            ),
        ],
      ),
    );
  }
}

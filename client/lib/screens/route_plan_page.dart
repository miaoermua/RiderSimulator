import 'package:flutter/material.dart';
import 'package:latlong2/latlong.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';
import '../widgets/delivery_map.dart';
import 'order_detail_page.dart';

/// 打开路线规划。手上没单也照开——地图上至少能看见自己在哪。
Future<void> showRoutePlan(
  BuildContext context, {
  required GameState state,
  required GameClient client,
}) {
  return Navigator.of(context).push<void>(
    MaterialPageRoute<void>(
      builder: (_) => RoutePlanPage(state: state, client: client),
    ),
  );
}

/// 路线规划：上半地图（取 → 送画圆弧），下半线性步骤（1 取 / 2 送…）。
///
/// 「线性」指的是把手上所有单的取送点摊平成一条执行顺序：先把待取的单都取了，
/// 再挨个送。和众包骑手的实际习惯一致——顺路的取货一起拿，比取一单送一单省里程。
class RoutePlanPage extends StatelessWidget {
  const RoutePlanPage({
    super.key,
    required this.state,
    required this.client,
  });

  final GameState state;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: state,
      builder: (context, _) {
        final steps = RoutePlanView._stepsOf(state);
        return Scaffold(
          appBar: AppBar(
            title: const Text('路线规划'),
            actions: [
              Padding(
                padding: const EdgeInsets.only(right: 12),
                child: Center(
                  child: Text(
                    '${RoutePlanView.activeOrders(state).length} 单 · ${steps.length} 个点',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                ),
              ),
            ],
          ),
          // 独立页面不需要自带关闭栏——AppBar 的返回就是退出方式。
          body: RoutePlanView(state: state, client: client),
        );
      },
    );
  }
}

/// 路线规划的**内容本体**：上半地图（取 → 送画圆弧），下半线性步骤（1 取 / 2 送…）。
///
/// 抽出来是为了让「独立页面」和「首页内嵌覆盖层」共用同一份实现。
/// 两种入口画的是同一个东西，各写一份迟早漂移（改了一边忘了另一边）。
///
/// 「线性」指的是把手上所有单的取送点摊平成一条执行顺序：先把待取的单都取了，
/// 再挨个送。和众包骑手的实际习惯一致——顺路的取货一起拿，比取一单送一单省里程。
class RoutePlanView extends StatelessWidget {
  const RoutePlanView({
    super.key,
    required this.state,
    required this.client,
    this.onClose,
  });

  final GameState state;
  final GameClient client;

  /// 非空时在顶部显示一条「关闭 + 计数」栏。内嵌覆盖层用它退出；
  /// 独立页面靠 AppBar 返回，传 `null` 即可。
  final VoidCallback? onClose;

  /// 手上所有进行中的单（待取 + 待送）。
  static List<OrderView> activeOrders(GameState state) => [
        ...state.pendingPickup,
        ...state.pendingDelivery,
      ];

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: state,
      builder: (context, _) {
        final active = activeOrders(state);
        final steps = _stepsOf(state);

        return Column(
          children: [
            if (onClose != null)
              Container(
                padding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
                decoration: const BoxDecoration(
                  border: Border(bottom: BorderSide(color: Colors.black12)),
                ),
                child: Row(
                  children: [
                    Icon(Icons.alt_route,
                        size: 18, color: Colors.grey.shade700),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text(
                        '路线规划 · ${active.length} 单 · ${steps.length} 个点',
                        style: const TextStyle(
                            fontSize: 13, fontWeight: FontWeight.w600),
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                    TextButton.icon(
                      onPressed: onClose,
                      icon: const Icon(Icons.close, size: 18),
                      label: const Text('关闭'),
                    ),
                  ],
                ),
              ),
            Expanded(
              flex: 5,
              child: active.isEmpty
                  ? _EmptyMap(state: state)
                  : DeliveryMap(
                      state: state,
                      orders: active,
                      showMerchants: false,
                      showMe: true,
                      // 弧线：多单叠在一起时，直线会糊成一团。
                      arcRoutes: true,
                      fitPadding: 44,
                    ),
            ),
            Expanded(
              flex: 4,
              child: steps.isEmpty
                  ? const Center(
                      child: Text(
                        '手上没有进行中的单。\n去大厅抢一单，这里就会列出取送顺序。',
                        textAlign: TextAlign.center,
                        style: TextStyle(color: Colors.grey),
                      ),
                    )
                  : ListView.separated(
                      padding: const EdgeInsets.fromLTRB(4, 8, 4, 16),
                      itemCount: steps.length + 1,
                      separatorBuilder: (_, i) => i == 0
                          ? const SizedBox.shrink()
                          : const Divider(height: 1),
                      itemBuilder: (context, i) {
                        if (i == 0) return const _PlanHeader();
                        return _StepTile(
                          no: i,
                          step: steps[i - 1],
                          previous: i >= 2 ? steps[i - 2].point : null,
                          me: state.me,
                          onTap: () => showOrderDetail(
                            context,
                            order: steps[i - 1].order,
                            state: state,
                            client: client,
                          ),
                        );
                      },
                    ),
            ),
          ],
        );
      },
    );
  }

  /// 摊平执行顺序：所有取货点在前，所有送达点在后。
  ///
  /// 待取单的送达点也在最后出现一次——取完才送得到，顺序不能反。
  /// 摊平执行顺序。方法私有：返回类型是私有的 `_Step`，
  /// 暴露成 public 会让 outside 无从使用（也会触发 lint）。
  static List<_Step> _stepsOf(GameState state) {
    final pickup = state.pendingPickup;
    final delivery = state.pendingDelivery;
    return [
      for (final o in pickup) _Step(order: o, pickup: true),
      for (final o in pickup) _Step(order: o, pickup: false),
      for (final o in delivery) _Step(order: o, pickup: false),
    ];
  }
}

class _Step {
  const _Step({required this.order, required this.pickup});

  final OrderView order;
  final bool pickup;

  LatLng get point => pickup
      ? LatLng(order.merchantLat, order.merchantLng)
      : LatLng(order.customerLat, order.customerLng);

  String get label => pickup ? '取' : '送';

  String get title => pickup ? order.merchantName : order.customerName;

  /// 取货点没有独立地址字段（服务端只给商家名），所以取这一行只标「到店取货」。
  String get address => pickup
      ? '到店取货'
      : (order.customerAddress.isEmpty
          ? order.customerName
          : order.customerAddress);

  Color get color => pickup ? const Color(0xFF1976D2) : const Color(0xFF2E7D32);
}

class _PlanHeader extends StatelessWidget {
  const _PlanHeader();

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(14, 4, 14, 8),
      child: Row(
        children: [
          const Icon(Icons.alt_route, size: 16, color: Colors.grey),
          const SizedBox(width: 6),
          Text(
            '建议顺序 · 先取后送',
            style: Theme.of(context)
                .textTheme
                .labelMedium
                ?.copyWith(color: Colors.grey),
          ),
        ],
      ),
    );
  }
}

/// 一行步骤：编号、取/送、地址、到下一个点的距离。
class _StepTile extends StatelessWidget {
  const _StepTile({
    required this.no,
    required this.step,
    required this.previous,
    required this.me,
    required this.onTap,
  });

  final int no;
  final _Step step;

  /// 上一步的点；第一步为空，距离就从「我」算起。
  final LatLng? previous;
  final RiderView? me;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final from = previous ?? (me == null ? null : LatLng(me!.lat, me!.lng));
    final meters = from == null
        ? null
        : const Distance().as(LengthUnit.Meter, from, step.point);
    final distance = meters == null
        ? null
        : meters >= 1000
            ? '${(meters / 1000).toStringAsFixed(1)}km'
            : '${meters.round()}m';

    return ListTile(
      onTap: onTap,
      leading: CircleAvatar(
        radius: 14,
        backgroundColor: step.color.withValues(alpha: 0.14),
        child: Text(
          '$no',
          style: TextStyle(
            fontSize: 12,
            fontWeight: FontWeight.bold,
            color: step.color,
          ),
        ),
      ),
      title: Row(
        children: [
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
            decoration: BoxDecoration(
              color: step.color.withValues(alpha: 0.12),
              borderRadius: BorderRadius.circular(8),
            ),
            child: Text(
              step.label,
              style: TextStyle(
                fontSize: 11,
                fontWeight: FontWeight.w700,
                color: step.color,
              ),
            ),
          ),
          const SizedBox(width: 6),
          Expanded(
            child: Text(
              step.title,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(fontWeight: FontWeight.w600),
            ),
          ),
        ],
      ),
      subtitle: Text(
        [
          step.address,
          if (distance != null) '距上一点 $distance',
          '订单 #${step.order.id}',
        ].join(' · '),
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
        style: const TextStyle(fontSize: 12),
      ),
      trailing: const Icon(Icons.chevron_right, size: 18),
    );
  }
}

/// 没单时的地图：只画自己，别让人以为页面挂了。
class _EmptyMap extends StatelessWidget {
  const _EmptyMap({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) => DeliveryMap(
        state: state,
        showMerchants: true,
        showMe: true,
        showBasemapPicker: true,
      );
}

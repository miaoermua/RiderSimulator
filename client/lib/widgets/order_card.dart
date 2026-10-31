import 'package:flutter/material.dart';
import 'package:latlong2/latlong.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../navigation_service.dart';
import 'app_toast.dart';
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
    // 这一单的抢单阶段。null = 还没开始；卡片据此决定画滑块还是等待/成功。
    final attempt =
        mode == OrderCardMode.hall ? state.claimAttemptFor(order.id) : null;

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
                        // 标题行只放「取货点标识 + 商家名 + 导航」。
                        //
                        // 标签（档位/背单/推荐/顺路/同点）挪到下面单独一行用
                        // `Wrap` 排——挤在同一个 Row 里时，标签一多加上导航图标
                        // 在窄屏上必然溢出（Row 里的固定宽子项没法压缩）。
                        // Wrap 会自动换行，多几个标签也不会挤坏商家名。
                        Row(
                          children: [
                            // 取货绿点。送货那行用橙点，两处颜色一一对应地图上的标记。
                            Container(
                              width: 8,
                              height: 8,
                              margin: const EdgeInsets.only(right: 7),
                              decoration: BoxDecoration(
                                color: Colors.green.shade600,
                                shape: BoxShape.circle,
                              ),
                            ),
                            Expanded(
                              child: Text(
                                order.merchantName,
                                style: theme.textTheme.titleMedium
                                    ?.copyWith(fontWeight: FontWeight.w600),
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                            ),
                            // 取货导航。与送货行末尾那个图标右边缘对齐
                            // （两行外层 padding 相同）。
                            _NavButton(
                              order: order,
                              target: NavTarget.pickup,
                              state: state,
                            ),
                          ],
                        ),
                        const SizedBox(height: 6),
                        Wrap(
                          spacing: 6,
                          runSpacing: 6,
                          children: [
                            if (mode == OrderCardMode.assign || order.forced)
                              _Chip(
                                text: state.assignForced ? '强制派单' : '平台派单',
                                color: Colors.redAccent,
                              ),
                            _TierChip(tier: order.tier),
                            if (order.backOrder)
                              const _Chip(text: '背单', color: Colors.teal),
                            // 推荐与顺路是**两个独立维度**，可以同时挂在同一张单上；
                            // 同取货/同送货也互斥不了（同一单可能既同店又同小区）。
                            if (order.recommended)
                              const _Chip(text: '推荐', color: Colors.deepOrange),
                            if (order.onTheWayPct case final pct?)
                              _Chip(text: '顺路 $pct%', color: Colors.indigo),
                            if (order.samePickup)
                              const _Chip(text: '同取货', color: Colors.cyan),
                            if (order.sameDropoff)
                              const _Chip(text: '同送货', color: Colors.cyan),
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
                          dotColor: Colors.orange.shade700,
                          label: '送',
                          text: order.customerAddress.isEmpty
                              ? order.customerName
                              : '${order.customerName} · ${order.customerAddress}',
                          trailing: _NavButton(
                            order: order,
                            target: NavTarget.dropoff,
                            state: state,
                          ),
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
                        // 等待/成功期间不画抢单倒计时：那时光看进度条会误以为
                        // 这一单还挂在墙上。失败后滑块回来，倒计时也一起回来。
                        if (mode == OrderCardMode.hall &&
                            (attempt == null ||
                                attempt.phase == ClaimPhase.failed))
                          _CountdownBar(
                              order: order, state: state, expired: expired),
                        if (widget.showActions &&
                            mode == OrderCardMode.hall &&
                            state.isRider) ...[
                          const SizedBox(height: 8),
                          _ClaimAction(
                            // gen 每次重试 +1：滑块里那个一次性的 `_fired` 锁
                            // 靠换 key 重建，失败后才能重新滑。
                            key: ValueKey('claim-${order.id}-${attempt?.gen ?? 0}'),
                            order: order,
                            state: state,
                            attempt: attempt,
                            expired: expired,
                            onConfirm: () {
                              if (state.beginClaim(order)) {
                                widget.client.claim(order.id);
                              }
                            },
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

  /// 取货点。展开的地图以它为中心——那是这张单的起点。
  LatLng get _pickupPoint {
    final o = widget.order;
    return LatLng(o.merchantLat, o.merchantLng);
  }

  /// 取送坐标是否都可用。
  ///
  /// 展开地图前先判一次：把 NaN / 越界值喂给 Leaflet，结果是**整块空白**，
  /// 看起来像地图坏了；明确说一句「缺坐标」反而更好懂。
  bool get _hasCoords {
    final o = widget.order;
    bool ok(double lat, double lng) =>
        lat.isFinite && lng.isFinite && lat.abs() <= 90 && lng.abs() <= 180;
    return ok(o.merchantLat, o.merchantLng) && ok(o.customerLat, o.customerLng);
  }

  /// 左侧竖条：**一个**里程按钮。
  ///
  /// 原本这里是「上箭头 / 距离 / 下箭头」：箭头切焦点、中间的距离跟着变，
  /// 读起来像两个里程入口，而且数字的含义随点什么漂移（一会儿是"我→取货点"，
  /// 一会儿是"我→送货点"），最快的理解方式反而是去猜。
  ///
  /// 合并后只有一件事：
  /// - 数字固定是**订单的取送里程**（取 → 送），是订单的固有属性；
  /// - 点一下展开地图，焦点落在取货点。
  ///
  /// 「我 → 取货点 / 我 → 送货点」的距离不再挂在竖条上——地图里本来就有骑手点，
  /// 那两个数放那儿才说得清。把三个数挤进 58px 宽的一条，谁都读不出来。
  Widget _rail(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final o = widget.order;
    return Container(
      width: 58,
      decoration: BoxDecoration(
        color: scheme.primary.withValues(alpha: 0.05),
        border: const Border(right: BorderSide(color: Colors.black12)),
      ),
      child: InkWell(
        onTap: () => setState(() => _mapOpen = !_mapOpen),
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            // 取 → 送：两个点是这张单的两端，箭头表示方向。
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                _dot(Colors.green.shade600),
                Icon(Icons.east, size: 11, color: Colors.grey.shade500),
                _dot(Colors.orange.shade700),
              ],
            ),
            const SizedBox(height: 3),
            Text(
              o.distanceLabel,
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
    );
  }

  static Widget _dot(Color c) => Container(
        width: 7,
        height: 7,
        decoration: BoxDecoration(color: c, shape: BoxShape.circle),
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
                child: !_hasCoords
                    ? const Center(
                        child: Text(
                          '这单缺少取送坐标，无法预览路线',
                          style: TextStyle(fontSize: 12, color: Colors.grey),
                        ),
                      )
                    : Stack(
                        children: [
                          Positioned.fill(
                            child: DeliveryMap(
                              state: widget.state,
                              orders: [widget.order],
                              showMerchants: false,
                              showMe: true,
                              showBasemapPicker: false,
                              focus: _pickupPoint,
                            ),
                          ),
                          // 只吃「点一下」，拖动 / 缩放仍然归地图。
                          Positioned.fill(
                            child: Material(
                              color: Colors.transparent,
                              child: InkWell(onTap: _openDetail),
                            ),
                          ),
                          // 左下角：说清这条线是什么。
                          //
                          // 项目里没有路网数据，画的是**取送两点之间的直线**。
                          // 不标注的话，用户会默认它是导航路线，进而以为
                          // 实际骑行也是这个距离/方向——这是最容易被误解的地方。
                          Positioned(
                            left: 8,
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
                                child: const Text(
                                  '位置连线 · 非道路导航',
                                  style: TextStyle(
                                      fontSize: 10, color: Colors.grey),
                                ),
                              ),
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
                                    Text('订单详情',
                                        style: TextStyle(fontSize: 11)),
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
        // 强制派单没有「拒绝」这个动作：藏掉按钮，别给一个必然失败的操作。
        // 指派单额度用尽时按钮挂着「拒不掉」而不是置灰：点下去会弹中心通知
        // 把原因说清楚（见 AssignOrderPage._reject），置灰只能让人猜。
        final noQuota = state.rejectQuotaExhausted;
        return Wrap(
          spacing: 8,
          children: [
            if (!state.assignForced)
              OutlinedButton(
                onPressed: onReject,
                child: Text(noQuota ? '拒不掉' : '拒绝'),
              ),
            FilledButton(
              onPressed: onAccept,
              child: Text(state.assignForced ? '同意' : '接受'),
            ),
          ],
        );
    }
  }
}

/// 一行「标签 + 内容」，用来放配送地址。
/// 订单卡上的一个地点（取货 / 送货）。
///
/// 标识统一成**彩色圆点**而不是图标：取货绿、送货橙。
/// 图标（`location_on`、`store`）在不同平台上形状差异大，
/// 而圆点只有颜色这一个维度，扫一眼就能分「这两个点是不是一回事」。
class _Leg extends StatelessWidget {
  const _Leg({
    required this.dotColor,
    required this.label,
    required this.text,
    this.trailing,
  });

  /// 圆点颜色。取货用绿、送货用橙——与地图上两个标记的颜色一致。
  final Color dotColor;
  final String label;
  final String text;

  /// 行末的动作（目前只有导航图标）。放在这里而不是外面包一层 Row，
  /// 是为了让图标与文字共用同一条基线、同一个右侧内边距。
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        // 8px 圆点。`margin-top: 5` 让它落在首行文字的中线上——
        // 用 Row 的 center 对齐反而会因为第二行换行而上下漂。
        Container(
          width: 8,
          height: 8,
          margin: const EdgeInsets.only(top: 5, right: 6),
          decoration: BoxDecoration(color: dotColor, shape: BoxShape.circle),
        ),
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
        if (trailing != null) trailing!,
      ],
    );
  }
}

/// 大厅倒计时条：越接近截止越红。
/// 大厅卡的抢单区。
///
/// 四种阶段共用同一块 44px 的位置，所以切换时卡片高度不变：
///
/// - 空闲 / 可重试失败：原来的 [`SlideToConfirm`]，样式和手势都不动；
/// - 提交中 / 确认中：隐藏滑块，换成连续旋转的 Loading + 文案；
/// - 成功：绿色勾选 +「抢单成功！」+「请按照要求进行配送」；
/// - 不可重试失败（被抢走 / 已失效）：收起滑块，只留原因，等待卡片被移除。
class _ClaimAction extends StatelessWidget {
  const _ClaimAction({
    super.key,
    required this.order,
    required this.state,
    required this.attempt,
    required this.expired,
    required this.onConfirm,
  });

  final OrderView order;
  final GameState state;
  final ClaimAttempt? attempt;
  final bool expired;
  final VoidCallback onConfirm;

  /// 与改动前的滑块同高，保证等待/成功状态不撑高卡片。
  static const double _height = 44;

  @override
  Widget build(BuildContext context) {
    final a = attempt;
    if (a != null && a.phase == ClaimPhase.submitting) {
      return _waiting('抢单中…', Colors.orange.shade800);
    }
    if (a != null && a.phase == ClaimPhase.confirming) {
      // 和"抢单中"分开：这时请求可能已经到了服务端，只是回包丢了，
      // 不能让骑手以为马上会有结果。
      return _waiting('正在确认抢单结果…', Colors.blueGrey.shade700);
    }
    if (a != null && a.phase == ClaimPhase.succeeded) {
      return _success(context);
    }
    if (a != null && a.phase == ClaimPhase.failed && !a.retryable) {
      return _gone(context, a.message ?? '这单已经不能抢了');
    }
    // 失败但可重试：原因放在滑块文案里，既显示原因又不改变高度。
    final reason = a?.message;
    return SizedBox(
      height: _height,
      width: double.infinity,
      child: SlideToConfirm(
        label: reason == null
            ? (expired ? '已截止，无法抢单' : '滑动抢单')
            : '$reason · 重新滑动',
        icon: Icons.pan_tool_alt,
        enabled: !expired,
        onConfirm: onConfirm,
      ),
    );
  }

  Widget _waiting(String text, Color color) {
    return SizedBox(
      height: _height,
      width: double.infinity,
      child: Center(
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            SizedBox(
              width: 18,
              height: 18,
              // value 为 null 时 Flutter 自己驱动连续旋转，不需要我们持有
              // AnimationController，页面销毁也就没有动画资源要释放。
              child: CircularProgressIndicator(
                strokeWidth: 2.4,
                strokeCap: StrokeCap.round,
                valueColor: AlwaysStoppedAnimation(color),
              ),
            ),
            const SizedBox(width: 10),
            Text(
              text,
              style: TextStyle(color: color, fontWeight: FontWeight.w600),
            ),
          ],
        ),
      ),
    );
  }

  Widget _success(BuildContext context) {
    final theme = Theme.of(context);
    return SizedBox(
      height: _height,
      width: double.infinity,
      // 一次性的入场缩放，不循环。父级每秒重建时 tween 已到位，不会重播。
      child: TweenAnimationBuilder<double>(
        tween: Tween(begin: 0.86, end: 1),
        duration: const Duration(milliseconds: 220),
        curve: Curves.easeOutBack,
        builder: (_, scale, child) =>
            Transform.scale(scale: scale, child: child),
        child: Center(
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(Icons.check_circle, size: 24, color: Colors.green.shade600),
              const SizedBox(width: 8),
              Column(
                mainAxisAlignment: MainAxisAlignment.center,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    '抢单成功！',
                    style: theme.textTheme.titleSmall?.copyWith(
                      color: Colors.green.shade700,
                      fontWeight: FontWeight.bold,
                    ),
                  ),
                  Text(
                    '请按照要求进行配送',
                    style: theme.textTheme.labelSmall
                        ?.copyWith(color: Colors.black54),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _gone(BuildContext context, String reason) {
    return SizedBox(
      height: _height,
      width: double.infinity,
      child: Center(
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.cancel_outlined, size: 18, color: Colors.grey.shade600),
            const SizedBox(width: 8),
            Flexible(
              child: Text(
                reason,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(color: Colors.grey.shade700),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

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

/// 订单卡上的导航图标。
///
/// **只吃自己的点击**：订单卡整块是「展开 / 收起地图」的热区，
/// 图标如果让事件冒泡上去，点导航会顺手把地图展开——两个动作粘在一起。
/// `IconButton` 本身是手势竞技场的参与者，父级 `InkWell` 不会同时触发；
/// 这里不需要再套 `GestureDetector` 去手动拦截。
class _NavButton extends StatelessWidget {
  const _NavButton({
    required this.order,
    required this.target,
    required this.state,
  });

  final OrderView order;
  final NavTarget target;
  final GameState state;

  /// 取货用商家坐标、送货用顾客坐标。**这两行不能写反**——
  /// 反了就是把骑手导到反方向去，比不给导航更糟。
  double get _lat =>
      target == NavTarget.pickup ? order.merchantLat : order.customerLat;
  double get _lng =>
      target == NavTarget.pickup ? order.merchantLng : order.customerLng;

  /// 传给地图应用的名称。取货用商家名；送货优先用地址
  /// （地图 App 的目的地点位更认街道信息），没地址才退回顾客名。
  String get _name {
    if (target == NavTarget.pickup) return order.merchantName;
    return order.customerAddress.isEmpty
        ? order.customerName
        : order.customerAddress;
  }

  bool get _usable => NavigationService.coordsUsable(_lat, _lng);

  Future<void> _go() async {
    final outcome = await NavigationService.navigate(
      app: state.navApp,
      lat: _lat,
      lng: _lng,
      name: _name,
      coordSystem: state.config.coordSystem,
    );
    if (outcome == NavOutcome.launched) return;
    // 只有失败才提示：成功时人已经被切到地图 App 了，toast 也看不见。
    showAppToast(
      switch (outcome) {
        NavOutcome.noCoords => '这单缺少${target.label}位置',
        NavOutcome.unavailable => '没找到可用的地图应用',
        NavOutcome.failed => '打开地图失败',
        NavOutcome.launched => '',
      },
      bad: true,
    );
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return IconButton(
      icon: const Icon(Icons.navigation_outlined, size: 18),
      color: scheme.primary,
      disabledColor: scheme.onSurface.withValues(alpha: 0.26),
      // 缺坐标时不禁用一个"哑巴"图标：禁用 + tooltip 说清为什么，
      // 比点了没反应更好懂。
      tooltip: _usable ? '导航到${target.label}点' : '缺少${target.label}坐标',
      visualDensity: VisualDensity.compact,
      // 40×40：视觉上仍是一枚小图标，触控区比 48 略小以免把行高撑开，
      // 但比图标本身大一圈，单手点得到。
      constraints: const BoxConstraints.tightFor(width: 40, height: 40),
      padding: EdgeInsets.zero,
      onPressed: _usable ? _go : null,
    );
  }
}

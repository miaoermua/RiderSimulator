import 'dart:async';

import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';
import '../widgets/order_card.dart';
import '../widgets/rider_drawer.dart';
import '../widgets/chat_sheet.dart';
import '../widgets/task_sheet.dart';
import 'assign_order_page.dart';
import 'notifications_page.dart';
import 'running_settings_sheet.dart';

/// 骑手端主界面。
///
/// 版式走众包骑手端的通行骨架：
/// 顶栏（折叠菜单 + 时钟 + 通知）/ 状态条 / 待抢·待取·待送 / 底栏（跑单设置 + 刷新）。
class RiderHomePage extends StatefulWidget {
  const RiderHomePage({super.key, required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  State<RiderHomePage> createState() => _RiderHomePageState();
}

class _RiderHomePageState extends State<RiderHomePage> {
  Timer? _ticker;

  /// 已经为哪一张指派单弹过确认界面——同一张不重复弹。
  int? _poppedFor;

  /// 已经提示过第几条错误，避免同一条 SnackBar 连着弹。
  int _seenErrorSeq = 0;

  @override
  void initState() {
    super.initState();
    widget.state.addListener(_onState);
    // 每秒重绘一次，让倒计时 / 时钟走字。
    _ticker = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted) setState(() {});
    });
  }

  @override
  void dispose() {
    widget.state.removeListener(_onState);
    _ticker?.cancel();
    super.dispose();
  }

  /// 服务端拒掉一次操作（比如人没到就点「到店」）时，当场弹一条。
  ///
  /// 光写进通知列表不够——骑手按完按钮没反应会更懵。
  void _complainOnce() {
    final err = widget.state.lastError;
    if (err == null || widget.state.errorSeq == _seenErrorSeq) return;
    _seenErrorSeq = widget.state.errorSeq;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      ScaffoldMessenger.of(context)
        ..hideCurrentSnackBar()
        ..showSnackBar(
          SnackBar(content: Text(err), duration: const Duration(seconds: 2)),
        );
    });
  }

  /// 收到系统指派就弹确认界面。导航不能放在 build 里，推到下一帧。
  void _onState() {
    _complainOnce();

    final id = widget.state.assignOrderId;
    if (id == null) {
      _poppedFor = null;
      return;
    }
    if (_poppedFor == id) return;
    _poppedFor = id;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || widget.state.assignOrderId != id) return;
      showAssignOrderPage(context, state: widget.state, client: widget.client);
    });
  }

  /// 下拉刷新与底部大按钮共用。
  ///
  /// 服务端没有单独的「刷新完成」回包，这里等一小段，让转圈有可感知的时长；
  /// 数据回来后 `AnimatedBuilder` 会自己重画，不需要额外通知。
  Future<void> _refresh() async {
    widget.client.resync();
    await Future<void>.delayed(const Duration(milliseconds: 650));
  }

  @override
  Widget build(BuildContext context) {
    final state = widget.state;
    return AnimatedBuilder(
      animation: state,
      builder: (context, _) {
        final hall = state.hall;
        final pickup = state.pendingPickup;
        final delivery = state.pendingDelivery;
        return DefaultTabController(
          length: 3,
          child: Scaffold(
            drawer: RiderDrawer(state: state, client: widget.client),
            appBar: AppBar(
              title: _Title(state: state),
              actions: [_BellButton(state: state)],
            ),
            body: Column(
              children: [
                _StatusStrip(state: state, client: widget.client),
                TabBar(
                  dividerColor: Colors.black12,
                  tabs: [
                    Tab(text: _tabLabel('待抢', hall.length)),
                    Tab(text: _tabLabel('待取', pickup.length)),
                    Tab(text: _tabLabel('待送', delivery.length)),
                  ],
                ),
                Expanded(
                  child: TabBarView(
                    children: [
                      _OrderList(
                        orders: hall,
                        mode: OrderCardMode.hall,
                        state: state,
                        client: widget.client,
                        empty: '大厅暂时没单，下拉刷新试试。',
                        onRefresh: _refresh,
                      ),
                      _OrderList(
                        orders: pickup,
                        mode: OrderCardMode.pickup,
                        state: state,
                        client: widget.client,
                        empty: '没有待取的单。去大厅抢一单吧。',
                        onRefresh: _refresh,
                      ),
                      _OrderList(
                        orders: delivery,
                        mode: OrderCardMode.delivery,
                        state: state,
                        client: widget.client,
                        empty: '没有待送的单。',
                        onRefresh: _refresh,
                      ),
                    ],
                  ),
                ),
              ],
            ),
            bottomNavigationBar: _BottomTools(
              state: state,
              client: widget.client,
              onRefresh: _refresh,
            ),
          ),
        );
      },
    );
  }

  static String _tabLabel(String name, int n) => n == 0 ? name : '$name $n';
}

class _Title extends StatelessWidget {
  const _Title({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    final color = switch (state.status) {
      ConnStatus.online => Colors.greenAccent,
      ConnStatus.connecting || ConnStatus.retrying => Colors.orangeAccent,
      _ => Colors.redAccent,
    };
    return Row(
      children: [
        Container(
          width: 10,
          height: 10,
          decoration: BoxDecoration(color: color, shape: BoxShape.circle),
        ),
        const SizedBox(width: 8),
        Text('第 ${state.day} 天 ${clockLabel(state.minuteOfDay)}'),
        const SizedBox(width: 10),
        Text(
          state.weatherLabel,
          style: Theme.of(context).textTheme.bodyMedium,
        ),
      ],
    );
  }
}

/// 顶栏通知铃铛，未读数是小红点。
class _BellButton extends StatelessWidget {
  const _BellButton({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    final n = state.unreadLogs;
    return IconButton(
      tooltip: '通知消息',
      onPressed: () => Navigator.of(context).push<void>(
        MaterialPageRoute<void>(
          builder: (_) => NotificationsPage(state: state),
        ),
      ),
      icon: n == 0
          ? const Icon(Icons.notifications_none)
          : Badge(label: Text('$n'), child: const Icon(Icons.notifications_none)),
    );
  }
}

/// 常驻状态条：出勤、模式、今日收入。
///
/// 竞品调研第一条结论就是「状态条必须常驻顶部」——骑手要随时确认
/// 自己是否在线、什么模式，否则接不到单都不知道为什么。
class _StatusStrip extends StatelessWidget {
  const _StatusStrip({required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    final me = state.me;
    if (me == null) {
      return Container(
        width: double.infinity,
        color: Colors.black12,
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
        child: const Text('观战中：你看到的是公开信息，无法抢单。'),
      );
    }
    final online = me.online;
    return Container(
      width: double.infinity,
      color: online ? Colors.green.withValues(alpha: 0.10) : Colors.black12,
      padding: const EdgeInsets.fromLTRB(12, 6, 12, 6),
      child: Row(
        children: [
          InkWell(
            borderRadius: BorderRadius.circular(14),
            onTap: () => client.setOnline(!online),
            child: Container(
              padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
              decoration: BoxDecoration(
                color: online ? Colors.green : Colors.grey,
                borderRadius: BorderRadius.circular(14),
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(
                    online ? Icons.wifi_tethering : Icons.wifi_tethering_off,
                    size: 14,
                    color: Colors.white,
                  ),
                  const SizedBox(width: 4),
                  Text(
                    online ? '在线' : '离线',
                    style: const TextStyle(
                      color: Colors.white,
                      fontSize: 12,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(width: 8),
          Text(
            '${me.speedMode.label} · ${state.config.isPeak ? '高峰' : '平峰'}',
            style: const TextStyle(fontSize: 12),
          ),
          const Spacer(),
          if (me.frozen) const _Badge('冻结', Colors.red),
          if (me.injured) const _Badge('受伤', Colors.orange),
          Text(
            '今日 ¥${yuan(me.takehomeCents ?? 0)} · ${me.completed} 单',
            style: const TextStyle(fontSize: 12, fontWeight: FontWeight.w600),
          ),
        ],
      ),
    );
  }
}

class _Badge extends StatelessWidget {
  const _Badge(this.text, this.color);

  final String text;
  final Color color;

  @override
  Widget build(BuildContext context) => Container(
        margin: const EdgeInsets.only(right: 6),
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
        decoration: BoxDecoration(
          color: color.withValues(alpha: 0.15),
          borderRadius: BorderRadius.circular(6),
        ),
        child: Text(text, style: TextStyle(color: color, fontSize: 11)),
      );
}

/// 三个 tab 共用的订单列表：连续滚动 + 下拉刷新。
///
/// 原先是「一屏固定张数 + 竖向翻页 + 上下页按钮」。竖向翻页和下拉刷新抢
/// 同一个手势，只能二选一；这里选了刷新——单子多了往下滚，想同步就往下拉，
/// 不用再去点底部按钮。
class _OrderList extends StatelessWidget {
  const _OrderList({
    required this.orders,
    required this.mode,
    required this.state,
    required this.client,
    required this.empty,
    required this.onRefresh,
  });

  final List<OrderView> orders;
  final OrderCardMode mode;
  final GameState state;
  final GameClient client;
  final String empty;

  /// 下拉刷新。Future 结束之前，顶上的刷新圈不会收。
  final Future<void> Function() onRefresh;

  @override
  Widget build(BuildContext context) {
    return RefreshIndicator(
      onRefresh: onRefresh,
      color: Theme.of(context).colorScheme.primary,
      backgroundColor: Theme.of(context).colorScheme.surface,
      displacement: 28,
      child: ListView.builder(
        // 不满一屏也要能下拉，否则空列表 / 单子少的时候拉不动。
        physics: const AlwaysScrollableScrollPhysics(),
        padding: const EdgeInsets.symmetric(vertical: 8),
        // 空态也占一个 item，省得分两条构建路径。
        itemCount: orders.isEmpty ? 1 : orders.length,
        itemBuilder: (context, i) => orders.isEmpty
            ? Padding(
                padding: const EdgeInsets.only(top: 140),
                child: Center(
                  child: Text(
                    empty,
                    style: const TextStyle(color: Colors.grey),
                  ),
                ),
              )
            : _card(context, orders[i]),
      ),
    );
  }

  Widget _card(BuildContext context, OrderView o) => Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          OrderCard(
            order: o,
            state: state,
            client: client,
            mode: mode,
          ),
          // 末端交付沟通：**任何到手单都能说**，不要求挂任务——
          // 这正是它和下面那个「任务对话」按钮的区别。
          if (mode != OrderCardMode.hall)
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
              child: OutlinedButton.icon(
                onPressed: () => showChatSheet(
                  context,
                  order: o,
                  state: state,
                  client: client,
                ),
                icon: const Icon(Icons.support_agent, size: 18),
                label: Text(_chatLabel(o)),
              ),
            ),
          // 送单路上要能顺手回顾客消息，别藏到别的页面去。
          if (mode != OrderCardMode.hall && o.task != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
              child: OutlinedButton.icon(
                onPressed: () => showTaskSheet(
                  context,
                  order: o,
                  state: state,
                  client: client,
                ),
                icon: const Icon(Icons.chat_bubble_outline, size: 18),
                label: Text('任务对话：${o.task!.name}'),
              ),
            ),
        ],
      );

  /// 「联系顾客」按钮上的字。
  ///
  /// 顾客开口是稀罕事（大约 5% 的单子），所以一来消息就值得在按钮上报一声，
  /// 免得骑手错过——尤其是带赔付的那种。
  String _chatLabel(OrderView o) {
    final has = (state.chatLines[o.id] ?? const <ChatLine>[])
        .any((l) => l.from == 'customer');
    return has ? '联系顾客 · 有新消息' : '联系顾客';
  }
}

/// 底部工具栏：跑单设置占两成宽，刷新占八成。
///
/// 设置是低频操作，做小：图标在上、文字在下、不带边框；
/// 刷新是跑单时一直想按的，做大，单手拇指好够。
class _BottomTools extends StatefulWidget {
  const _BottomTools({
    required this.state,
    required this.client,
    required this.onRefresh,
  });

  final GameState state;
  final GameClient client;
  final Future<void> Function() onRefresh;

  @override
  State<_BottomTools> createState() => _BottomToolsState();
}

class _BottomToolsState extends State<_BottomTools> {
  /// 刷新进行中：图标换成转圈，顺便按住重复点击。
  bool _busy = false;

  Future<void> _refresh() async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await widget.onRefresh();
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return BottomAppBar(
      height: 64,
      padding: const EdgeInsets.fromLTRB(8, 6, 8, 6),
      child: Row(
        children: [
          // 两成：图标 + 文字的素按钮，没有边框也没有底色。
          Expanded(
            flex: 2,
            child: InkWell(
              borderRadius: BorderRadius.circular(10),
              onTap: () =>
                  showRunningSettings(context, state: widget.state, client: widget.client),
              child: Column(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  Icon(Icons.tune, size: 19, color: scheme.onSurfaceVariant),
                  const SizedBox(height: 2),
                  Text(
                    '跑单设置',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(fontSize: 10, color: scheme.onSurfaceVariant),
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(width: 8),
          // 八成：一颗大按钮，转圈就是刷新动效。
          Expanded(
            flex: 8,
            child: FilledButton.tonal(
              onPressed: _busy ? null : _refresh,
              style: FilledButton.styleFrom(
                minimumSize: const Size.fromHeight(46),
                shape: RoundedRectangleBorder(
                  borderRadius: BorderRadius.circular(12),
                ),
              ),
              child: AnimatedSwitcher(
                duration: const Duration(milliseconds: 180),
                child: _busy
                    ? const SizedBox(
                        key: ValueKey('busy'),
                        width: 18,
                        height: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Row(
                        key: ValueKey('idle'),
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Icon(Icons.refresh, size: 20),
                          SizedBox(width: 6),
                          Text('刷新'),
                        ],
                      ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

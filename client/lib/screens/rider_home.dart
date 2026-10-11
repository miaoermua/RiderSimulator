import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';
import '../widgets/order_card.dart';
import '../widgets/rider_drawer.dart';
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

  /// 收到系统指派就弹确认界面。导航不能放在 build 里，推到下一帧。
  void _onState() {
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

  Future<void> _refresh() async {
    widget.client.resync();
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(
        const SnackBar(content: Text('已请求刷新'), duration: Duration(seconds: 1)),
      );
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
                        // 一页几张照众包骑手端的全面屏版式来：
                        // 待抢 3 张、待取 / 待送 2 张，一屏看全不用滑。
                        perPage: 3,
                      ),
                      _OrderList(
                        orders: pickup,
                        mode: OrderCardMode.pickup,
                        state: state,
                        client: widget.client,
                        empty: '没有待取的单。去大厅抢一单吧。',
                        perPage: 2,
                      ),
                      _OrderList(
                        orders: delivery,
                        mode: OrderCardMode.delivery,
                        state: state,
                        client: widget.client,
                        empty: '没有待送的单。',
                        perPage: 2,
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

/// 一页订单列表。三个 tab 共用，只有 [mode] 与 [perPage] 不同。
///
/// **分页而不是长滚动**：全面屏上照众包骑手端的版式，一屏看全固定张数
/// （待抢 3 张、待取 / 待送 2 张），比一列长列表更快做完「抢不抢」的判断。
/// 竖向滑动翻页，另配上下页按钮，单手够得着。
///
/// 这里不挂 `RefreshIndicator`：下拉手势已经被翻页吃掉，
/// 刷新走底部工具栏那颗按钮（见 `_BottomTools`）。
class _OrderList extends StatefulWidget {
  const _OrderList({
    required this.orders,
    required this.mode,
    required this.state,
    required this.client,
    required this.empty,
    required this.perPage,
  });

  final List<OrderView> orders;
  final OrderCardMode mode;
  final GameState state;
  final GameClient client;
  final String empty;

  /// 一页放几张。
  final int perPage;

  @override
  State<_OrderList> createState() => _OrderListState();
}

class _OrderListState extends State<_OrderList> {
  final PageController _pager = PageController();
  int _page = 0;

  int get _pageCount => widget.orders.isEmpty
      ? 0
      : (widget.orders.length + widget.perPage - 1) ~/ widget.perPage;

  @override
  void dispose() {
    _pager.dispose();
    super.dispose();
  }

  @override
  void didUpdateWidget(covariant _OrderList old) {
    super.didUpdateWidget(old);
    // 单被抢走 / 送完会让页数变少，把当前页拉回最后一页，
    // 否则会停在一页空白上，看着像「单没了」。
    final last = _pageCount - 1;
    if (_page <= last) return;
    final target = last < 0 ? 0 : last;
    _page = target;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || !_pager.hasClients) return;
      _pager.jumpToPage(target);
    });
  }

  @override
  Widget build(BuildContext context) {
    if (widget.orders.isEmpty) return _emptyView();
    return Column(
      children: [
        Expanded(
          child: PageView.builder(
            controller: _pager,
            scrollDirection: Axis.vertical,
            itemCount: _pageCount,
            onPageChanged: (i) => setState(() => _page = i),
            itemBuilder: (context, i) => _pageBody(i),
          ),
        ),
        if (_pageCount > 1) _pagerBar(),
      ],
    );
  }

  /// 第 [page] 页：固定 [perPage] 个槽位，等分高度。
  ///
  /// 卡片按自然高度画，放不下时整体等比缩小（`FittedBox.scaleDown`）——
  /// 小屏塞三张卡时宁可字小一点，也不能 overflow 把底部按钮顶掉。
  Widget _pageBody(int page) {
    final start = page * widget.perPage;
    final end = math.min(start + widget.perPage, widget.orders.length);
    final slice = widget.orders.sublist(start, end);
    return Column(
      children: [
        for (final o in slice)
          Expanded(
            child: LayoutBuilder(
              builder: (context, c) => FittedBox(
                fit: BoxFit.scaleDown,
                alignment: Alignment.topCenter,
                child: SizedBox(width: c.maxWidth, child: _card(o)),
              ),
            ),
          ),
      ],
    );
  }

  Widget _card(OrderView o) => Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          OrderCard(
            order: o,
            state: widget.state,
            client: widget.client,
            mode: widget.mode,
          ),
          // 送单路上要能顺手回顾客消息，别藏到别的页面去。
          if (widget.mode != OrderCardMode.hall && o.task != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
              child: OutlinedButton.icon(
                onPressed: () => showTaskSheet(
                  context,
                  order: o,
                  state: widget.state,
                  client: widget.client,
                ),
                icon: const Icon(Icons.chat_bubble_outline, size: 18),
                label: Text('任务对话：${o.task!.name}'),
              ),
            ),
        ],
      );

  Widget _emptyView() => RefreshIndicator(
        onRefresh: () async => widget.client.resync(),
        child: ListView(
          physics: const AlwaysScrollableScrollPhysics(),
          children: [
            const SizedBox(height: 140),
            Center(
              child: Text(
                widget.empty,
                style: const TextStyle(color: Colors.grey),
              ),
            ),
          ],
        ),
      );

  /// 页脚：上一页 / 第几页 / 下一页。
  Widget _pagerBar() => Row(
        children: [
          IconButton(
            tooltip: '上一页',
            onPressed: _page > 0
                ? () => _pager.previousPage(
                      duration: const Duration(milliseconds: 220),
                      curve: Curves.easeOut,
                    )
                : null,
            icon: const Icon(Icons.keyboard_arrow_up),
          ),
          Expanded(
            child: Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                for (var i = 0; i < _pageCount; i++)
                  Container(
                    width: i == _page ? 18 : 6,
                    height: 6,
                    margin: const EdgeInsets.symmetric(horizontal: 3),
                    decoration: BoxDecoration(
                      color: i == _page
                          ? Theme.of(context).colorScheme.primary
                          : Colors.black26,
                      borderRadius: BorderRadius.circular(3),
                    ),
                  ),
              ],
            ),
          ),
          IconButton(
            tooltip: '下一页',
            onPressed: _page < _pageCount - 1
                ? () => _pager.nextPage(
                      duration: const Duration(milliseconds: 220),
                      curve: Curves.easeOut,
                    )
                : null,
            icon: const Icon(Icons.keyboard_arrow_down),
          ),
        ],
      );
}

/// 底部工具栏：跑单设置 + 刷新。
class _BottomTools extends StatelessWidget {
  const _BottomTools({
    required this.state,
    required this.client,
    required this.onRefresh,
  });

  final GameState state;
  final GameClient client;
  final Future<void> Function() onRefresh;

  @override
  Widget build(BuildContext context) {
    return BottomAppBar(
      height: 56,
      padding: EdgeInsets.zero,
      child: Row(
        children: [
          Expanded(
            child: TextButton.icon(
              onPressed: () =>
                  showRunningSettings(context, state: state, client: client),
              icon: const Icon(Icons.tune),
              label: const Text('跑单设置'),
            ),
          ),
          Container(width: 1, height: 24, color: Colors.black12),
          Expanded(
            child: TextButton.icon(
              onPressed: onRefresh,
              icon: const Icon(Icons.refresh),
              label: const Text('刷新'),
            ),
          ),
        ],
      ),
    );
  }
}

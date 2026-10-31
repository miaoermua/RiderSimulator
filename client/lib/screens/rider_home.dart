import 'dart:async';

import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../location_service.dart';
import '../protocol.dart';
import '../widgets/app_toast.dart';
import '../widgets/notice_sheet.dart';
import '../widgets/order_card.dart';
import '../widgets/rider_drawer.dart';
import 'assign_order_page.dart';
import 'notifications_page.dart';
import 'route_plan_page.dart';
import 'running_settings_sheet.dart';

/// 骑手端主界面。
///
/// 版式走众包骑手端的通行骨架：
/// 顶栏（折叠菜单 + 上线/下线 + 通知）/ 状态条 / 待抢·待取·待送 / 底栏（跑单设置 + 刷新）。
class RiderHomePage extends StatefulWidget {
  const RiderHomePage({super.key, required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  State<RiderHomePage> createState() => _RiderHomePageState();
}

class _RiderHomePageState extends State<RiderHomePage> {
  /// 页内路线规划是否打开。
  ///
  /// 只在「平台偏好」模式下用：那个模式要求点路线规划**不跳页**，
  /// 而是在首页盖一层地图。关闭时这个值一定是 false，
  /// 走的是独立页面（`showRoutePlan`），两种入口互不影响。
  ///
  /// 用条件构建（`if (_routeOpen)`）而不是 `Offstage`：关闭时
  /// `DeliveryMap` 整个从树上摘掉，地图控制器随之销毁，不会留定时器或订阅。
  bool _routeOpen = false;

  // TODO(audio): 平台偏好模式的**内置音效**接这里。
  //
  // 计划是「新单进大厅」「指派单到达」各放一声短提示。本期**刻意不实现**，
  // 原因不是没时间：`AlertCenter` 已经有一整套音效体系（系统铃声 + 可换的
  // 本地音频 + 自定义提示音），再叠一套必须先定清楚三件事——
  // 两套谁优先、同时触发时怎么避免双响、静音开关管不管这一套。
  // 这些没定之前先接上，用户会听到两声重叠的提示。
  // 接入点就在这里：`_watchOnline` / `_onState` 旁边，那里已经在监听
  // 这些状态变化了。

  Timer? _ticker;

  /// 已经为哪一张指派单弹过确认界面——同一张不重复弹。
  int? _poppedFor;

  /// 已经提示过第几条错误，避免同一条提示连着弹。
  int _seenErrorSeq = 0;

  /// 已经弹过第几次「拒单结果」，重放事件时不重复弹。
  int _seenRejectSeq = 0;

  /// 已经提示过的定位失败原因。
  String? _seenLocationError;

  /// 上一次看到的在线状态：用来在上下线时弹一句。
  bool? _onlineBefore;

  @override
  void initState() {
    super.initState();
    widget.state.addListener(_onState);
    // 抢单 ack 超时后，状态机需要再要一份快照来确认归属。回调注入，
    // 避免 `GameState` 反过来依赖 WebSocket 客户端。
    widget.state.onResyncRequested = widget.client.resync;
    // 进主界面才开定位：登录页没必要问权限，也不想在没连上时就上报。
    LocationService.instance.attach(widget.client, widget.state);
    // 每秒重绘一次，让倒计时 / 时钟走字。
    _ticker = Timer.periodic(const Duration(seconds: 1), (_) {
      if (!mounted) return;
      _complainLocation();
      setState(() {});
    });
  }

  @override
  void dispose() {
    widget.state.removeListener(_onState);
    widget.state.onResyncRequested = null;
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
      showAppToast(err, bad: true);
    });
  }

  /// 上下线各弹一句。上线提醒是平台合规要求的那条；下线只说清楚状态。
  ///
  /// 首帧不弹（`_onlineBefore == null`）——进页面时本来就在线，没必要补一句。
  void _watchOnline() {
    final online = widget.state.me?.online;
    if (online != null && _onlineBefore != null && online != _onlineBefore) {
      showAppToast(
        online ? '交警部门提醒：带好安全盔骑行不吃亏' : '已下线，注意休息',
        bad: false,
      );
    }
    _onlineBefore = online;
  }

  /// 定位出问题（没权限 / 系统定位没开）时当场说一声。
  ///
  /// 不说的话「我」会一直停在老位置，骑手只会觉得地图坏了。
  void _complainLocation() {
    final err = LocationService.instance.error;
    if (err == null || err == _seenLocationError) return;
    _seenLocationError = err;
    showAppToast(err, bad: true);
  }

  /// 每拒掉一单都明说「现在拒了几次」——额度只有这里看得见，
  /// 藏在通知列表里等于没有。用序号驱动，重放旧事件不会重复弹。
  void _showRejectNotice() {
    final state = widget.state;
    if (state.rejectNoticeSeq <= _seenRejectSeq) return;
    _seenRejectSeq = state.rejectNoticeSeq;

    final used = state.rejectNoticeUsed;
    final limit = state.rejectNoticeLimit;
    final left = state.rejectNoticeLeft;
    final forcedOffline = state.rejectNoticeForcedOffline;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      showDialog<void>(
        context: context,
        builder: (ctx) => AlertDialog(
          title: Text(forcedOffline ? '已被强制下线' : '已拒绝一单'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                limit > 0
                    ? '今日已拒 $used/$limit 次，还剩 $left 次'
                    : '今日已拒 $used 次（不限次数）',
              ),
              if (forcedOffline) ...[
                const SizedBox(height: 10),
                const Text(
                  '拒单额度已用尽，今天不能再自己上线；手上的单照常送完，平台派的单一律按强制派单处理。',
                  style: TextStyle(color: Colors.redAccent),
                ),
              ],
            ],
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.of(ctx).pop(),
              child: const Text('知道了'),
            ),
          ],
        ),
      );
    });
  }

  /// 收到系统派单就弹确认界面。导航不能放在 build 里，推到下一帧。
  void _onState() {
    _complainOnce();
    _watchOnline();
    _showRejectNotice();

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

  /// 打开路线规划。两种模式的落点不同：
  ///
  /// - 平台偏好**关**：跳独立页面（原本的行为，不动）；
  /// - 平台偏好**开**：在首页盖一层（任务要求"不跳转独立页面"）。
  ///
  /// 两条路都只是"看"——**不碰订单状态、派单状态或骑手容量**。
  /// 覆盖层是纯展示组件，关掉就把整棵子树摘掉。
  void _openRoute() {
    if (widget.state.platformPref) {
      setState(() => _routeOpen = true);
    } else {
      showRoutePlan(context, state: widget.state, client: widget.client);
    }
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
              title: _Title(state: state, client: widget.client),
              actions: [_BellButton(state: state)],
            ),
            // Stack：底层是正常首页，上层是「平台偏好」模式下的页内路线图。
            // 覆盖层只在 `_routeOpen` 时构建，关闭即销毁（见字段注释）。
            body: Stack(
              children: [
                Column(
                  children: [
                    _NoticeStrip(state: state),
                    _CapacityStrip(state: state),
                    Row(
                      children: [
                        Expanded(
                          child: TabBar(
                            dividerColor: Colors.black12,
                            tabs: [
                              Tab(text: _tabLabel('待抢', hall.length)),
                              Tab(text: _tabLabel('待取', pickup.length)),
                              Tab(text: _tabLabel('待送', delivery.length)),
                            ],
                          ),
                        ),
                        // 平台偏好模式：路线规划从底栏挪到「待送」右侧。
                        // 放在 TabBar 的 Row 里而不是浮动按钮，是为了让它
                        // 跟着标签栏一起滚开/固定，位置稳定、不挡订单卡。
                        if (state.platformPref)
                          Padding(
                            padding: const EdgeInsets.only(right: 4),
                            child: IconButton(
                              icon: const Icon(Icons.alt_route, size: 20),
                              tooltip: '路线规划',
                              onPressed: _openRoute,
                            ),
                          ),
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
                            platformPref: state.platformPref,
                          ),
                          _OrderList(
                            orders: pickup,
                            mode: OrderCardMode.pickup,
                            state: state,
                            client: widget.client,
                            empty: '没有待取的单。去大厅抢一单吧。',
                            onRefresh: _refresh,
                            platformPref: state.platformPref,
                          ),
                          _OrderList(
                            orders: delivery,
                            mode: OrderCardMode.delivery,
                            state: state,
                            client: widget.client,
                            empty: '没有待送的单。',
                            onRefresh: _refresh,
                            platformPref: state.platformPref,
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
                if (_routeOpen)
                  Positioned.fill(
                    child: Material(
                      color: Theme.of(context).colorScheme.surface,
                      child: SafeArea(
                        child: RoutePlanView(
                          state: state,
                          client: widget.client,
                          onClose: () => setState(() => _routeOpen = false),
                        ),
                      ),
                    ),
                  ),
              ],
            ),
            bottomNavigationBar: BottomTools(
              state: state,
              client: widget.client,
              onRefresh: _refresh,
              onRoutePlan: _openRoute,
            ),
          ),
        );
      },
    );
  }

  static String _tabLabel(String name, int n) => n == 0 ? name : '$name $n';
}

/// 顶栏左侧：连接状态点 + 上线 / 下线按钮。
///
/// 原来这里是「绿点 + 第 N 天 + 时钟 + 天气」。时间天气不再常驻（骑手看单
/// 不看表），但**连接状态必须留着**——断线时抢单没反应，得能一眼看出是网络
/// 问题。绿点因此并进了按钮左边，按钮本身点了就弹出「上线 / 下线」。
class _Title extends StatelessWidget {
  const _Title({required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    final me = state.me;
    final online = me?.online ?? false;
    // 被强制下线的人不能自己上线：顶栏直接说清楚，别让他反复点「上线」撞错误。
    final forcedOffline = me?.forcedOffline ?? false;
    final tone = forcedOffline
        ? Colors.redAccent
        : (online ? Colors.green : Colors.grey);
    final conn = switch (state.status) {
      ConnStatus.online => Colors.greenAccent,
      ConnStatus.connecting || ConnStatus.retrying => Colors.orangeAccent,
      _ => Colors.redAccent,
    };
    return Align(
      alignment: Alignment.centerLeft,
      child: InkWell(
        borderRadius: BorderRadius.circular(16),
        onTap: me == null ? null : () => _openSheet(context, me),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
          decoration: BoxDecoration(
            color: tone.withValues(alpha: 0.12),
            borderRadius: BorderRadius.circular(16),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              // 连接状态点：绿 = 连着，黄 = 重连中，红 = 断了。
              Container(
                width: 8,
                height: 8,
                decoration: BoxDecoration(color: conn, shape: BoxShape.circle),
              ),
              const SizedBox(width: 6),
              Icon(
                online ? Icons.wifi_tethering : Icons.wifi_tethering_off,
                size: 16,
                color: tone,
              ),
              const SizedBox(width: 4),
              Text(
                me == null
                    ? '观战'
                    : (forcedOffline ? '强制下线' : (online ? '上线' : '下线')),
                style: TextStyle(
                  fontSize: 13,
                  fontWeight: FontWeight.w600,
                  color: tone,
                ),
              ),
              if (me != null) const Icon(Icons.arrow_drop_down, size: 18),
            ],
          ),
        ),
      ),
    );
  }

  /// 弹出「上线 / 下线」。选中的那一项打勾，点另一项就切换。
  void _openSheet(BuildContext context, RiderView me) {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (sheet) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              title: const Text('出勤状态'),
              subtitle: Text(me.forcedOffline
                  ? '拒单额度已用尽，今天被强制下线'
                  : (me.online ? '现在在线，大厅的单可以抢' : '现在离线，接不到新单')),
            ),
            const Divider(height: 1),
            ListTile(
              enabled: !me.forcedOffline,
              leading: Icon(
                Icons.wifi_tethering,
                color: me.forcedOffline ? Colors.grey : Colors.green,
              ),
              title: const Text('上线'),
              subtitle: Text(me.forcedOffline ? '今天不能再上线，明天再来' : '开始接单'),
              trailing: me.online ? const Icon(Icons.check) : null,
              onTap: () {
                Navigator.of(sheet).pop();
                if (!me.online) client.setOnline(true);
              },
            ),
            ListTile(
              leading: const Icon(Icons.wifi_tethering_off, color: Colors.grey),
              title: const Text('下线'),
              subtitle: const Text('暂停接单，手上的单还在'),
              trailing: me.online ? null : const Icon(Icons.check),
              onTap: () {
                Navigator.of(sheet).pop();
                if (me.online) client.setOnline(false);
              },
            ),
            const SizedBox(height: 8),
          ],
        ),
      ),
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
      tooltip: '通知和活动中心',
      onPressed: () => Navigator.of(context).push<void>(
        MaterialPageRoute<void>(
          builder: (_) => NotificationsPage(state: state),
        ),
      ),
      icon: n == 0
          ? const Icon(Icons.notifications_none)
          : Badge(
              label: Text('$n'), child: const Icon(Icons.notifications_none)),
    );
  }
}

/// 首页通知条：烽火台写的单条公告，点开看全文。
///
/// 这里原来是「在线 / 模式 / 今日收入」的绿色条子，现在整条换掉了：
/// - 在线（含上下线）就在顶栏按钮上，本来就是重复的；
/// - 收入与单量去侧边抽屉 / 战报看，跑单时盯着数字没意义；
/// - 「冻结 / 受伤 / 观战」这类**现在根本跑不了单**的状态仍然顶在最上面，
///   不然骑手会以为 App 坏了。它们不该被通知条盖掉，所以单独一行。
class _NoticeStrip extends StatelessWidget {
  const _NoticeStrip({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    final me = state.me;
    final notice = state.notice;
    final rows = <Widget>[];

    if (me == null) {
      rows.add(Container(
        width: double.infinity,
        color: Colors.black12,
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
        child: const Text('观战中：你看到的是公开信息，无法抢单。'),
      ));
    }

    if (notice != null) {
      rows.add(InkWell(
        onTap: () => showNoticeSheet(context, state: state, notice: notice),
        child: Container(
          width: double.infinity,
          color: Colors.amber.withValues(alpha: 0.16),
          padding: const EdgeInsets.fromLTRB(12, 8, 8, 8),
          child: Row(
            children: [
              Icon(Icons.campaign_outlined,
                  size: 18, color: Colors.orange.shade800),
              const SizedBox(width: 8),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      notice.title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 13,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    const SizedBox(height: 1),
                    Text(
                      notice.body,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: 11,
                        color: Colors.grey.shade700,
                      ),
                    ),
                  ],
                ),
              ),
              // 关联了活动：给个奖杯提示，让骑手知道点进去还有进度可看。
              if (notice.activityId != null)
                Padding(
                  padding: const EdgeInsets.only(left: 4),
                  child: Icon(Icons.emoji_events_outlined,
                      size: 16, color: Colors.deepOrange.shade400),
                ),
              const Icon(Icons.chevron_right, size: 18, color: Colors.black38),
            ],
          ),
        ),
      ));
    }

    if (me != null && (me.frozen || me.injured)) {
      rows.add(Container(
        width: double.infinity,
        color: Colors.red.withValues(alpha: 0.08),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
        child: Row(
          children: [
            if (me.frozen) const _Badge('冻结', Colors.red),
            if (me.injured) const _Badge('受伤', Colors.orange),
            Expanded(
              child: Text(
                '现在跑不了单，先联系调度。',
                style: TextStyle(fontSize: 12, color: Colors.red.shade900),
              ),
            ),
          ],
        ),
      ));
    }

    if (rows.isEmpty) return const SizedBox.shrink();
    return Column(children: rows);
  }
}

/// 满载提示条。只在**已接不了新单**时出现，常驻显示反而会变成噪音。
///
/// 「满」由服务端快照里的 `held_cap`（已含策略覆盖）判定；上限为 0 也算满载。
/// 这里只是提示，抢单能不能成仍由服务端 `check_claimable` 裁决。
class _CapacityStrip extends StatelessWidget {
  const _CapacityStrip({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    final me = state.me;
    if (me == null || !state.myAtCapacity) return const SizedBox.shrink();
    final cap = state.myHeldCap ?? 0;
    final text = cap == 0
        ? '当前接单上限为 0：先把手上 ${me.held.length} 单送完'
        : '已满载（${me.held.length}/$cap）：送完一单才能再接';
    return Container(
      width: double.infinity,
      color: Colors.orange.withValues(alpha: 0.14),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      child: Row(
        children: [
          Icon(Icons.shopping_bag_outlined,
              size: 16, color: Colors.orange.shade900),
          const SizedBox(width: 6),
          Expanded(
            child: Text(
              text,
              style: TextStyle(fontSize: 12, color: Colors.orange.shade900),
            ),
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
    this.platformPref = false,
  });

  final List<OrderView> orders;
  final OrderCardMode mode;
  final GameState state;
  final GameClient client;
  final String empty;

  /// 下拉刷新。Future 结束之前，顶上的刷新圈不会收。
  final Future<void> Function() onRefresh;

  /// 平台偏好模式：空态的插画位换成预留占位块。
  final bool platformPref;

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
                padding: const EdgeInsets.only(top: 80, left: 24, right: 24),
                child: Center(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      // 插画位。
                      //
                      // 平台偏好模式要求这里换成"预留的插画"——所以给一个
                      // 与插画同宽的占位块，两种模式之间切换时下面的文案不会跳位置。
                      //
                      // TODO(illustration): 平台偏好专属插画待接入。
                      // 换的时候把 `_IllustrationPlaceholder` 换成
                      // `Image.asset('assets/illustrations/...')` 即可，
                      // 尺寸保持 200 宽，别动下面的间距。
                      if (platformPref)
                        const _IllustrationPlaceholder()
                      else
                        // 插画是透明底的线稿，跟着主题走不会突兀。
                        Image.asset(
                          'assets/illustrations/empty_orders.png',
                          width: 200,
                          filterQuality: FilterQuality.medium,
                        ),
                      const SizedBox(height: 12),
                      Text(
                        empty,
                        textAlign: TextAlign.center,
                        style: const TextStyle(color: Colors.grey),
                      ),
                    ],
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
          // 任务对话已经并进「在线对话」——订单卡的操作区那一个按钮就够了，
          // 不必在卡片下面再挂一条「任务对话」的入口。见 showChatSheet。
        ],
      );
}

/// 底部工具栏：跑单设置与路线规划各占两成，刷新占六成。
///
/// 前两个是低频操作，做小：图标在上、文字在下、不带边框；
/// 刷新是跑单时一直想按的，做大，单手拇指好够。
///
/// 公开（而不是 `_BottomTools`）只是为了**布局测试能单独 pump 它**——
/// 整个首页挂着定时器和连接状态，为了量一次按钮宽度去 pump 全页太重。
/// 业务上它仍然只服务这个页面。
@visibleForTesting
class BottomTools extends StatefulWidget {
  const BottomTools({
    required this.state,
    required this.client,
    required this.onRefresh,
    required this.onRoutePlan,
  });

  final GameState state;
  final GameClient client;
  final Future<void> Function() onRefresh;

  /// 点「路线规划」。具体是跳页还是盖一层由父级决定——
  /// 底栏不该知道平台偏好，它只负责把点击传上去。
  final VoidCallback onRoutePlan;

  @override
  State<BottomTools> createState() => BottomToolsState();
}

class BottomToolsState extends State<BottomTools> {
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

  /// 底栏左侧的小入口：图标在上、文字在下，不带边框也不带底色。
  Widget _toolButton({
    required IconData icon,
    required String label,
    required VoidCallback onTap,
  }) {
    final color = Theme.of(context).colorScheme.onSurfaceVariant;
    return InkWell(
      borderRadius: BorderRadius.circular(10),
      onTap: onTap,
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          Icon(icon, size: 19, color: color),
          const SizedBox(height: 2),
          Text(
            label,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(fontSize: 10, color: color),
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return BottomAppBar(
      height: 64,
      padding: const EdgeInsets.fromLTRB(8, 6, 8, 6),
      child: Row(
        children: [
          // 设置与规划是低频入口，各占两成；刷新是高频动作，占余下的六成。
          Expanded(
            flex: 2,
            child: _toolButton(
              icon: Icons.tune,
              label: '跑单设置',
              onTap: () => showRunningSettings(
                context,
                state: widget.state,
                client: widget.client,
              ),
            ),
          ),
          // 平台偏好开启时这一格让出来：路线规划已经挪到「待送」右侧，
          // 底栏再留一个等于同一个动作给两个入口，宽度还更好分给刷新。
          if (!widget.state.platformPref) ...[
            const SizedBox(width: 4),
            Expanded(
              flex: 2,
              child: _toolButton(
                icon: Icons.alt_route,
                label: '路线规划',
                onTap: widget.onRoutePlan,
              ),
            ),
          ],
          const SizedBox(width: 8),
          // 刷新按钮。平台偏好开启时它让出一截：
          //
          // - `flex` 6 → 5：剩余宽度里少分一份，按钮本身变窄；
          // - 右侧再补 20px 留白：右边缘向左收，不至于顶到屏幕最右边——
          //   那一带是拇指回勾最容易误触的地方，而且上方 TabBar 右侧还挂着
          //   路线规划图标，底栏右边缘留白能跟它形成一条视觉竖线。
          //
          // 只动宽度分配，不动高度、不改字号、不缩小图标：
          // 触控面积仍然是一整条 46 高的按钮，比原来只窄一点点。
          Expanded(
            flex: widget.state.platformPref ? 5 : 6,
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
          if (widget.state.platformPref) const SizedBox(width: 20),
        ],
      ),
    );
  }
}

/// 平台偏好模式下的插画占位。
///
/// 尺寸对齐原来的插画（200 宽），两种模式切换时下面的文案不会跳位置。
///
/// TODO(illustration): 接入平台偏好专属插画后删掉这个组件，
/// 换成 `Image.asset(...)`，宽度保持 200。
class _IllustrationPlaceholder extends StatelessWidget {
  const _IllustrationPlaceholder();

  @override
  Widget build(BuildContext context) {
    final c = Theme.of(context).colorScheme;
    return Container(
      width: 200,
      height: 120,
      decoration: BoxDecoration(
        color: c.surfaceContainerHighest.withValues(alpha: 0.5),
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: c.outlineVariant),
      ),
      alignment: Alignment.center,
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.image_outlined, size: 26, color: c.onSurfaceVariant),
          const SizedBox(height: 6),
          Text(
            '插画位（待接入）',
            style: TextStyle(fontSize: 12, color: c.onSurfaceVariant),
          ),
        ],
      ),
    );
  }
}

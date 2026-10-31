import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'alert_event.dart';
import 'navigation_service.dart';
import 'protocol.dart';

/// 连接状态。UI 用它决定顶栏圆点颜色与重连提示。
enum ConnStatus { idle, connecting, online, retrying, closed }

/// 一条给用户看的滚动消息。
class LogLine {
  LogLine(this.text, {this.kind = LogKind.info, DateTime? at})
      : at = at ?? DateTime.now();

  final String text;
  final LogKind kind;
  final DateTime at;
}

enum LogKind { info, good, bad, money, chat }

/// 一笔抢单请求的阶段。
///
/// 刻意不合成一个 `isLoading`：等待、确认、成功、失败在界面上的表现完全不同，
/// 而且"确认中"必须能和"提交中"区分开——前者表示**服务端可能已经处理完了**，
/// 只是客户端没收到回包。
enum ClaimPhase {
  /// 请求已发出，等 ack。
  submitting,

  /// 等太久没等到 ack，正在用快照确认这一单到底归谁。
  confirming,

  /// 服务端已明确把这一单分配给本人。
  succeeded,

  /// 服务端明确拒绝，或快照证明没有抢到。
  failed,
}

/// 一张订单的抢单尝试。
///
/// 按订单存，不按页面存：骑手可能先后滑两张卡，两单的等待/成功/失败互不影响。
class ClaimAttempt {
  ClaimAttempt({
    required this.order,
    required this.gen,
    required this.index,
    this.phase = ClaimPhase.submitting,
  });

  /// 提交那一刻的订单快照。成功后这张卡会从大厅被移走，但成功态还要在原位
  /// 停留一会儿，所以不能依赖 `hall` 里还能找到它。
  final OrderView order;

  /// 每次重试 +1。界面上拿它当 key，把滑块里那个一次性的 `_fired` 锁重置掉。
  final int gen;

  /// 提交时在大厅里的下标，快照整表替换后用来把成功卡片插回原位。
  final int index;

  ClaimPhase phase;

  /// 服务端错误码（本地网络错误为 null）。
  String? code;

  /// 给人看的原因。
  String? message;

  /// 失败后还能不能再滑一次。
  bool retryable = true;

  /// 「确认中」是否已经发过一次 resync，避免无限重发。
  bool resynced = false;

  bool get active =>
      phase == ClaimPhase.submitting || phase == ClaimPhase.confirming;
  bool get succeeded => phase == ClaimPhase.succeeded;
}

/// 钱包里的一条收支。金额单位是分，负数是支出（罚款 / 抽水）。
class WalletEntry {
  WalletEntry({
    required this.at,
    required this.title,
    required this.cents,
    required this.kind,
  });

  final DateTime at;
  final String title;
  final int cents;

  /// `order` / `fine` / `activity` 之类的来源，只用来挑图标与颜色。
  final String kind;

  bool get income => cents >= 0;
}

/// 客户端世界状态。
///
/// **这是 UI 的唯一数据源**：网络层只负责把帧喂给 [applyFrame]，
/// UI 只监听本对象。所有解析失败都在这里被吞掉并记一条日志，绝不崩界面。
class GameState extends ChangeNotifier {
  /// 抢单相关的节奏都做成入参：生产用默认值，测试可以压到毫秒级。
  GameState({
    this.claimAckTimeout = const Duration(seconds: 8),
    this.claimConfirmTimeout = const Duration(seconds: 8),
    this.claimSuccessHold = const Duration(milliseconds: 1500),
    this.claimGoneHold = const Duration(milliseconds: 1200),
  });

  // ---------------------------------------------------------------------
  // 本地界面偏好（只影响骑手自己看到的版式）
  // ---------------------------------------------------------------------

  /// 「平台偏好」开关。
  ///
  /// 开了之后首页换一套版式：插画位换成预留块、路线规划从底栏挪到
  /// 「待送」右边、点开是页内地图而不是跳新页面。
  ///
  /// **它和派单半径没有关系**——半径是服务端的业务配置，改了会影响
  /// 能不能看见单；这里只是骑手自己喜不喜欢这个版式。所以它存在本地、
  /// 不上报服务端，也不参与任何业务判断。
  bool _platformPref = false;
  bool get platformPref => _platformPref;

  static const _kPlatformPref = 'platform_pref_enabled';

  /// 启动时读一次。读失败按默认（关）处理——一个界面偏好不值得让 App 起不来。
  Future<void> loadPlatformPref() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      _platformPref = prefs.getBool(_kPlatformPref) ?? false;
      notifyListeners();
    } catch (e) {
      debugPrint('platform_pref: 读取失败，按默认关闭处理 $e');
    }
  }

  Future<void> setPlatformPref(bool v) async {
    if (_platformPref == v) return;
    _platformPref = v;
    // 先通知再落盘：界面立刻切过去，写盘失败也不该让按钮卡在那里。
    notifyListeners();
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setBool(_kPlatformPref, v);
    } catch (e) {
      debugPrint('platform_pref: 保存失败 $e');
    }
  }

  /// 默认导航应用。
  ///
  /// 只存一个枚举名（`system` / `amap` / ...），读的时候按当前平台过滤一遍：
  /// 在 iPhone 上选了 Apple 地图、换成 Android 后读出来会回落到「系统默认」，
  /// 而不是记着一个这台机器上根本打不开的选项。
  NavApp _navApp = NavApp.system;
  NavApp get navApp => _navApp;

  static const _kNavApp = 'default_nav_app';

  Future<void> loadNavApp() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      final saved = NavApp.parse(prefs.getString(_kNavApp));
      _navApp = NavApp.forPlatform().contains(saved) ? saved : NavApp.system;
      notifyListeners();
    } catch (e) {
      debugPrint('nav_app: 读取失败，按系统默认处理 $e');
    }
  }

  Future<void> setNavApp(NavApp v) async {
    if (_navApp == v) return;
    _navApp = v;
    // 先通知再落盘：下一次点导航图标要立刻用新选择，不等写盘。
    notifyListeners();
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setString(_kNavApp, v.name);
    } catch (e) {
      debugPrint('nav_app: 保存失败 $e');
    }
  }

  // ---------------------------------------------------------------------
  // 连接 / 身份
  // ---------------------------------------------------------------------
  ConnStatus status = ConnStatus.idle;
  String serverUrl = kDefaultServerUrl;
  String role = 'spectator';
  int? riderId;
  String? riderKey;

  /// 上一次看到的出勤状态。null = 还没见过——首帧与重连不补一条提醒。
  bool? _lastOnline;

  // ---------------------------------------------------------------------
  // 世界
  // ---------------------------------------------------------------------
  int day = 1;
  int elapsedMin = 0;
  int simMinute = 0;
  String weather = 'clear';
  String weatherLabel = '晴';
  double weatherSpeedFactor = 1;

  ConfigView config = ConfigView.fromJson(const {});
  final Map<int, RiderView> riders = {};
  List<MerchantView> merchants = const [];

  List<OrderView> hall = [];
  List<OrderView> activeOrders = [];
  List<DayLedgerView> ledgers = const [];

  /// 钱包流水（本地累积）。服务端只给日结汇总，没有逐笔明细接口，
  /// 所以这里把每一笔 `rider_finance` 记下来；重连 / 重进 app 不清空。
  final List<WalletEntry> walletEntries = [];

  /// 平台活动（含已下架的）。进行中的见 [runningActivities]。
  List<ActivityView> activities = const [];

  /// 进行中的活动。活动中心只列这些。
  List<ActivityView> get runningActivities =>
      activities.where((a) => a.active).toList();

  /// 首页通知（烽火台写的单条公告）。`null` = 没写，首页不显示条子。
  NoticeView? notice;

  /// 每条订单的任务对话（绑在附加任务上）。
  ///
  /// 与 [chatLines] 分开存：服务端是两条通道，判定语义不同。UI 侧把两者
  /// 混成一条时间线，见 [timelineFor]。
  final Map<int, List<TaskLine>> taskLines = {};

  /// 每条订单的**末端会话**（末端交付沟通，与附加任务无关）。
  ///
  /// 服务端那边时效一天，日结时清库；本地缓存跟着在 `day_closed` 时清空，
  /// 见 [applyFrame]。
  final Map<int, List<ChatLine>> chatLines = {};

  /// 每条订单的「在线对话」时间线：末端会话 + 任务对话，按到达顺序混排。
  ///
  /// 单开一份而不是每次 merge 两个列表：同一分钟内的先后顺序只有到达顺序
  /// 知道，merge 会把它弄丢（骑手会觉得「我刚说的话跑到上面去了」）。
  final Map<int, List<ChatEntry>> _timelines = {};

  /// 一条订单的对话（含任务消息）。没说过话就是空的。
  List<ChatEntry> timelineFor(int orderId) => _timelines[orderId] ?? const [];

  final List<LogLine> logs = [];

  /// 服务端最近一次 tick 的本地落地时刻，用于把分钟数插值成平滑时钟。
  DateTime _syncedAt = DateTime.now();
  int _syncedMinute = 0;
  int tickIntervalMs = 1000;

  // ---------------------------------------------------------------------
  // 派生
  // ---------------------------------------------------------------------

  /// 插值后的「当前模拟分钟」。UI 每秒刷新一次即可。
  int get nowMinute {
    final interval = tickIntervalMs <= 0 ? 1000 : tickIntervalMs;
    final drift =
        DateTime.now().difference(_syncedAt).inMilliseconds ~/ interval;
    return _syncedMinute + drift;
  }

  int get minuteOfDay => nowMinute % 1440;

  RiderView? get me => riderId == null ? null : riders[riderId!];

  /// 我手上还没送完的单。
  List<OrderView> get myOrders {
    final id = riderId;
    if (id == null) return const [];
    return activeOrders
        .where((o) => o.assignedTo == id && o.status.isActive)
        .toList()
      ..sort((a, b) => a.claimDeadlineMin.compareTo(b.claimDeadlineMin));
  }

  /// 待取货：已接单、还没取餐（骑手阶段 = 去商家）。
  List<OrderView> get pendingPickup =>
      myOrders.where((o) => o.status == OrderStatus.assigned).toList();

  /// 待送达：已取餐、正在配送（骑手阶段 = 去客户）。
  List<OrderView> get pendingDelivery =>
      myOrders.where((o) => o.status == OrderStatus.pickedUp).toList();

  /// 按 id 找单，先大厅后手上。
  OrderView? orderById(int id) {
    for (final o in hall) {
      if (o.id == id) return o;
    }
    for (final o in activeOrders) {
      if (o.id == id) return o;
    }
    return null;
  }

  /// 大厅里还剩多少毫秒可抢。负数表示已过期。
  int claimRemainingMs(OrderView o) =>
      (o.claimDeadlineMin - nowMinute) *
      (tickIntervalMs <= 0 ? 1000 : tickIntervalMs);

  bool get isRider => riderId != null;

  // ---------------------------------------------------------------------
  // 地图底图
  // ---------------------------------------------------------------------

  /// 手动选的底图。`null` = 跟随服务端下发的坐标系（默认）。
  ///
  /// 只存在客户端，不上报——换底图是「换个角度看」，不改世界的坐标系。
  CoordSystem? basemapOverride;

  /// 当前该用的底图坐标系。
  CoordSystem get basemap => basemapOverride ?? config.coordSystem;

  /// 底图与下发坐标不一致：点位会整体偏移几百米。
  bool get basemapMismatch => basemap != config.coordSystem;

  void setBasemapOverride(CoordSystem? cs) {
    if (basemapOverride == cs) return;
    basemapOverride = cs;
    notifyListeners();
  }

  // ---------------------------------------------------------------------
  // 指派单（控制端强制指派）
  // ---------------------------------------------------------------------

  /// 待确认的指派单 id。非空时骑手端要弹出确认界面。
  int? assignOrderId;

  /// 这一单的派单方式：`offer` = 指派单（可拒），`forced` = 强制派单（只能同意）。
  ///
  /// 单独存一份是因为推送比订单快照先到——[assignOrder] 可能还是旧的那份。
  String? _assignMode;
  DateTime? _assignAt;

  /// 当前待确认的是不是强制派单。
  bool get assignForced => (_assignMode ?? assignOrder?.assignMode) == 'forced';

  /// 确认窗口。玩法设计 §4.3：30 秒内不确认则自动拒绝。
  ///
  /// 按**真实秒**（挂钟）计，不随模拟时间倍率缩放——这是留给玩家反应的窗口。
  static const assignWindow = Duration(seconds: 30);

  /// 指派单还能犹豫多少毫秒。
  int get assignRemainingMs {
    final at = _assignAt;
    if (at == null) return 0;
    final left = assignWindow.inMilliseconds -
        DateTime.now().difference(at).inMilliseconds;
    return left < 0 ? 0 : left;
  }

  /// 待确认的指派单本体（可能还没同步到，见 [assignOrderId] 的兜底逻辑）。
  OrderView? get assignOrder {
    final id = assignOrderId;
    return id == null ? null : orderById(id);
  }

  /// 关闭指派单确认界面（接受或拒绝之后都要调）。
  void resolveAssign() {
    if (assignOrderId == null && _assignAt == null && _assignMode == null) {
      return;
    }
    assignOrderId = null;
    _assignMode = null;
    _assignAt = null;
    notifyListeners();
  }

  /// 今日还剩几次拒单额度。`null` = 不限（服务端 `max_rejects_per_day == 0`）。
  int? get rejectQuotaLeft {
    final limit = config.maxRejectsPerDay;
    if (limit <= 0) return null;
    final left = limit - (me?.rejectsToday ?? 0);
    return left < 0 ? 0 : left;
  }

  /// 拒单额度已用尽：系统塞来的指派单**拒不掉**，只能接下。
  bool get rejectQuotaExhausted => rejectQuotaLeft == 0;

  // ---------------------------------------------------------------------
  // 背单容量（只用于展示；接单是否成功由服务端裁决）
  // ---------------------------------------------------------------------

  /// 自己的**生效**背单上限：快照里的 `held_cap`（已含策略覆盖）优先，
  /// 老服务端没下发时退回配置值。`null` = 还不知道自己是谁。
  int? get myHeldCap => effectiveHeldCap(me, config.backorderMax);

  /// 自己是否已满载（手上单量 >= 生效上限）。上限为 0 也算满载：
  /// 那种状态只能把手上的送完，不能再接新单。
  bool get myAtCapacity => riderAtCapacity(me, config.backorderMax);

  /// 「手上/上限」的展示串，例如 `2/3`。
  String get myHeldLabel => heldCapacityLabel(me, config.backorderMax);

  // ---------------------------------------------------------------------
  // 拒单结果弹窗
  // ---------------------------------------------------------------------

  /// 每拒掉一单 +1，首页靠它判断「是不是新的一次」，避免重复弹。
  int rejectNoticeSeq = 0;

  /// 这次拒单之后的已用次数 / 当日上限（0 = 不限）。
  int rejectNoticeUsed = 0;
  int rejectNoticeLimit = 0;

  /// 这次拒单是否把额度用尽、当场被强制下线。
  bool rejectNoticeForcedOffline = false;

  /// 今日还剩几次额度（`null` = 不限），弹窗文案用。
  int? get rejectNoticeLeft {
    if (rejectNoticeLimit <= 0) return null;
    final left = rejectNoticeLimit - rejectNoticeUsed;
    return left < 0 ? 0 : left;
  }

  // ---------------------------------------------------------------------
  // 通知
  // ---------------------------------------------------------------------

  /// 未读通知数，顶栏铃铛上的小红点。
  int unreadLogs = 0;

  /// 最近一条被服务端拒掉的操作，比如「还没到位置」。
  ///
  /// 通知列表里也有一份，但骑手点了按钮更想要当场反馈——
  /// [errorSeq] 每来一条错误就 +1，界面靠它判断「是不是新的一条」。
  String? lastError;
  int errorSeq = 0;

  /// 需要惊动骑手的事件（指派单、新活动）从这里出去。
  ///
  /// [GameState] 只管"发生了什么"，不 import 任何原生插件——
  /// 真正的弹通知 / 播音效由 `main.dart` 接到 [AlertCenter] 上，
  /// 单测里这个回调是 null，什么都不会发生。
  void Function(RiderAlert alert)? onAlert;

  void markLogsRead() {
    if (unreadLogs == 0) return;
    unreadLogs = 0;
    notifyListeners();
  }

  // ---------------------------------------------------------------------
  // 入口
  // ---------------------------------------------------------------------

  void setStatus(ConnStatus s) {
    status = s;
    notifyListeners();
  }

  void note(String text, {LogKind kind = LogKind.info}) {
    logs.insert(0, LogLine(text, kind: kind));
    if (logs.length > 200) logs.removeLast();
    unreadLogs++;
    notifyListeners();
  }

  // ---------------------------------------------------------------------
  // 抢单：按订单隔离的状态机
  //
  // 这里只管「这一单现在处于什么阶段」，不碰手势。滑块仍然留在
  // `SlideToConfirm` 里，卡片按阶段决定画滑块、Loading、成功还是失败。
  // ---------------------------------------------------------------------

  /// ack 迟迟不来，就从「提交中」转「确认中」的等待时间。
  final Duration claimAckTimeout;

  /// 「确认中」再等一轮的时间。到点只补发一次 resync，不认定失败。
  final Duration claimConfirmTimeout;

  /// 成功后卡片留在原位置的时间（此时待取列表里已经有同一单）。
  final Duration claimSuccessHold;

  /// 已被抢走 / 已失效的卡片多留一会儿，让人看清原因再移除。
  final Duration claimGoneHold;

  /// 每单一条，键是订单 id。
  final Map<int, ClaimAttempt> claims = {};

  final Map<int, Timer> _claimTimers = {};

  /// 已经播过成功音效的订单。快照 / 重连 / 事件重放都不该再响。
  final Set<int> _celebrated = {};

  int _claimGen = 0;
  bool _disposed = false;

  /// 结果不确定时把现有快照再要一份。由首页接到 `GameClient.resync`。
  void Function()? onResyncRequested;

  ClaimAttempt? claimAttemptFor(int orderId) => claims[orderId];

  /// 发起抢单。返回 false 表示这次没把请求发出去（离线 / 重复）。
  bool beginClaim(OrderView order) {
    final cur = claims[order.id];
    if (cur != null && cur.phase != ClaimPhase.failed) return false;
    final idx = hall.indexWhere((o) => o.id == order.id);
    final a = ClaimAttempt(
      order: order,
      gen: ++_claimGen,
      index: idx < 0 ? hall.length : idx,
    );
    claims[order.id] = a;
    if (status != ConnStatus.online) {
      _failClaim(order.id, null, message: '网络未连接，请重连后再试');
      note('网络未连接，请重连后再试', kind: LogKind.bad);
      return false;
    }
    _armClaimTimer(order.id, claimAckTimeout, () => _onAckTimeout(order.id));
    notifyListeners();
    return true;
  }

  void _armClaimTimer(int orderId, Duration d, VoidCallback cb) {
    _claimTimers.remove(orderId)?.cancel();
    _claimTimers[orderId] = Timer(d, () {
      _claimTimers.remove(orderId);
      if (_disposed) return;
      cb();
    });
  }

  /// 超时不是失败：服务端可能已经处理完，只是回包丢了。
  void _onAckTimeout(int orderId) {
    final a = claims[orderId];
    if (a == null || a.phase != ClaimPhase.submitting) return;
    a.phase = ClaimPhase.confirming;
    notifyListeners();
    onResyncRequested?.call();
    _armClaimTimer(
      orderId,
      claimConfirmTimeout,
      () => _onConfirmTimeout(orderId),
    );
  }

  void _onConfirmTimeout(int orderId) {
    final a = claims[orderId];
    if (a == null || a.phase != ClaimPhase.confirming) return;
    // 只补一次 resync。之后保持「结果未知」：滑块不回来、不许重复提交，
    // 等重连或下一次快照对账。
    if (a.resynced) return;
    a.resynced = true;
    onResyncRequested?.call();
    _armClaimTimer(
      orderId,
      claimConfirmTimeout,
      () => _onConfirmTimeout(orderId),
    );
  }

  /// 服务端明确把单分给了本人。
  ///
  /// [announce] 为 false 时只画成功态、只响一声，不写成功文案——调用方
  /// （`order_claimed` 事件）自己会写一行"抢到了"，避免一次抢单刷两遍屏。
  void _resolveClaimSuccess(int orderId, {bool announce = true}) {
    final a = claims[orderId];
    if (a == null || a.phase == ClaimPhase.succeeded) return;
    a
      ..phase = ClaimPhase.succeeded
      ..code = null
      ..message = null;
    _claimTimers.remove(orderId)?.cancel();
    if (_celebrated.add(orderId)) {
      onAlert?.call(
        const RiderAlert(
          kind: AlertKind.claimSuccess,
          title: '抢单成功',
          body: '🎉 抢单成功！恭喜你成功获得这笔订单，请按照平台要求及时前往商家取货，'
              '并按照订单信息完成配送。祝你配送顺利！',
        ),
      );
    }
    if (announce) {
      note(
        '🎉 抢单成功！请按照平台要求及时前往商家取货，并按照订单信息完成配送。',
        kind: LogKind.good,
      );
    }
    _armClaimTimer(orderId, claimSuccessHold, () => _retireClaim(orderId));
    notifyListeners();
  }

  /// 成功态停留结束：只收走大厅里那张「复制卡」，待取列表不动。
  void _retireClaim(int orderId) {
    hall.removeWhere((o) => o.id == orderId);
    claims.remove(orderId);
    notifyListeners();
  }

  void _failClaim(int orderId, String? code, {String? message}) {
    final a = claims[orderId];
    if (a == null || a.phase == ClaimPhase.succeeded) return;
    final (retryable, gone) = _classifyClaimFailure(code);
    a
      ..phase = ClaimPhase.failed
      ..code = code
      ..retryable = retryable
      ..message = (message != null && message.isNotEmpty)
          ? message
          : ErrCode.humanize(code);
    _claimTimers.remove(orderId)?.cancel();
    if (gone) {
      // 这单已经不可能再抢：先让人看清原因，再把卡片收走。
      _armClaimTimer(orderId, claimGoneHold, () {
        hall.removeWhere((o) => o.id == orderId);
        claims.remove(orderId);
        notifyListeners();
      });
      onResyncRequested?.call();
    }
    if (code == ErrCode.outOfRange) onResyncRequested?.call();
    notifyListeners();
  }

  static (bool retryable, bool gone) _classifyClaimFailure(String? code) {
    switch (code) {
      case ErrCode.alreadyClaimed:
      case ErrCode.orderNotFound:
      case ErrCode.orderExpired:
      case ErrCode.wrongPhase:
        return (false, true);
      default:
        return (true, false);
    }
  }

  /// 用快照对账。
  ///
  /// 快照是权威的：只要单在我的 active 里，就是成功；还留在大厅就不是我的。
  /// 这里**不动**已经处于失败态的卡片，避免 resync 把失败原因冲掉。
  void _reconcileClaims() {
    if (claims.isEmpty) return;
    final mine = riderId;
    for (final id in claims.keys.toList()) {
      final a = claims[id];
      if (a == null) continue;
      final isMine = activeOrders.any(
        (o) => o.id == id && o.status.isActive && o.assignedTo == mine,
      );
      if (isMine) {
        _resolveClaimSuccess(id, announce: a.phase != ClaimPhase.succeeded);
        continue;
      }
      final pending = hall.any((o) => o.id == id);
      switch (a.phase) {
        case ClaimPhase.submitting:
        case ClaimPhase.confirming:
          if (pending && a.phase == ClaimPhase.confirming) {
            _failClaim(id, null, message: '抢单没有成功，请重试');
          } else if (!pending) {
            _failClaim(id, ErrCode.orderExpired);
          }
        case ClaimPhase.succeeded:
          // 快照把大厅整表替换了，把成功卡片插回原位，别让它提前闪没。
          if (!pending) {
            final at = a.index < 0
                ? 0
                : (a.index > hall.length ? hall.length : a.index);
            hall.insert(
              at,
              _copyOrder(a.order,
                  status: OrderStatus.assigned, assignedTo: mine),
            );
          }
        case ClaimPhase.failed:
          if (a.retryable && !pending) claims.remove(id);
      }
    }
  }

  @override
  void dispose() {
    _disposed = true;
    for (final t in _claimTimers.values) {
      t.cancel();
    }
    _claimTimers.clear();
    super.dispose();
  }

  /// 吃一帧。返回后 UI 会被通知一次。
  void applyFrame(ServerFrame frame) {
    switch (frame.event) {
      case Ev.welcome:
        _applyWelcome(Welcome.fromJson(frame.payload));
      case Ev.snapshot:
        _applySnapshot(Snapshot.fromJson(frame.payload), announce: true);
      case Ev.ack:
        _applyAck(Ack.fromJson(frame.payload));
      case Ev.error:
        lastError = ErrCode.humanize(reqStr(frame.payload['code']));
        errorSeq++;
        note('服务端错误：$lastError', kind: LogKind.bad);
      case Ev.pong:
        _touch(frame.payload);
      case Ev.tick:
        _touch(frame.payload);
      case Ev.orderBroadcast:
        _applyOrderBroadcast(frame.payload);
      case Ev.orderClaimed:
        _applyOrderClaimed(frame.payload);
      case Ev.orderPickedUp:
        _applyOrderPickedUp(frame.payload);
      case Ev.orderDelivered:
        _applyOrderDelivered(frame.payload);
      case Ev.orderExpired:
        _removeOrder(reqInt(frame.payload['order_id']));
      case Ev.assignRejected:
        _applyAssignRejected(frame.payload);
      case Ev.riderStateChanged:
        _applyRiderState(frame.payload);
      case Ev.riderFinance:
        _applyFinance(frame.payload);
      case Ev.fineIssued:
        note(
          '罚款 ${yuan(reqInt(frame.payload['amount_cents']))} 元：'
          '${reqStr(frame.payload['reason'], '违规')}',
          kind: LogKind.bad,
        );
      case Ev.activitiesChanged:
        // 快照只在连接时来一次，烽火台改活动靠这条事件推过来。
        final before = activities.map((a) => a.id).toSet();
        activities = listOf(frame.payload['activities'], ActivityView.fromJson);
        _announceNewActivities(before);
      case Ev.noticeChanged:
        // 烽火台改文字 / 清空。快照只在连接时来一次，不接这条就得重连。
        notice = frame.payload['notice'] is Map
            ? NoticeView.fromJson(asMap(frame.payload['notice']))
            : null;
        if (notice != null) {
          note('通知：${notice!.title}', kind: LogKind.info);
        }
      case Ev.activityProgress:
        // 每完成一单推一条：活动页的进度条实时动。
        final rid = reqInt(frame.payload['rider_id']);
        final updated = listOf(
          frame.payload['activities'],
          ActivityProgressView.fromJson,
        );
        final rider = riders[rid];
        if (rid == riderId && rider != null) {
          riders[rid] = _copyRider(rider, activities: updated);
        }
      case Ev.activityReward:
        // 奖金已经进账（服务端自动发），这里只负责让人知道。
        note(
          '活动奖励 +${yuan(reqInt(frame.payload['amount_cents']))} 元：'
          '${reqStr(frame.payload['title'], '平台活动')}'
          '（${reqInt(frame.payload['completed'])}/'
          '${reqInt(frame.payload['target_orders'])} 单）',
          kind: LogKind.money,
        );
      case Ev.weatherChanged:
        _applyWeather(frame.payload);
      case Ev.configChanged:
        _applyConfig(frame.payload);
      case Ev.aiDecision:
        _applyAiDecision(frame.payload);
      case Ev.taskMessage:
        _applyTaskMessage(frame.payload);
      case Ev.chatMessage:
        _applyChatMessage(frame.payload);
      case Ev.dayClosed:
        _applyDayClosed(frame.payload);
        // 会话时效一天：服务端日结时清了库，本地缓存跟着清，
        // 否则翻回昨天的单，还能看见早就该没了的话。
        chatLines.clear();
        taskLines.clear();
        _timelines.clear();
      default:
        break; // 未知事件：忽略，向前兼容
    }
    _watchOnline();
    notifyListeners();
  }

  // ---------------------------------------------------------------------
  // 传输层
  // ---------------------------------------------------------------------

  void _applyWelcome(Welcome w) {
    role = w.role;
    riderId = w.riderId;
    riderKey = w.riderKey;
    // 换号 / 重连后重新认一次出勤状态，别拿上一个骑手的 online 去比。
    _lastOnline = null;
    tickIntervalMs = w.tickIntervalMs;
    _applySnapshot(w.snapshot, announce: false);
    note(
      riderId == null ? '已作为观战者进入' : '已上线：${me?.name ?? riderKey}',
      kind: LogKind.good,
    );
  }

  void _applySnapshot(Snapshot s, {required bool announce}) {
    day = s.day;
    elapsedMin = s.elapsedMin;
    simMinute = s.simMinute;
    _syncedMinute = s.elapsedMin;
    _syncedAt = DateTime.now();
    weather = s.weather;
    weatherLabel = s.weatherLabel;
    weatherSpeedFactor = s.weatherSpeedFactor;
    config = s.config;
    tickIntervalMs = s.config.tickIntervalMs;
    riders
      ..clear()
      ..addEntries(s.riders.map((r) => MapEntry(r.id, r)));
    merchants = s.merchants;
    hall = List.of(s.hall);
    activeOrders = List.of(s.activeOrders);
    ledgers = s.lastDayLedgers;
    activities = s.activities;
    notice = s.notice;
    // 快照会整表替换 hall / activeOrders，正在进行的抢单必须跟着对账。
    _reconcileClaims();
    if (announce) note('已同步世界状态');
  }

  void _applyAck(Ack a) {
    if (a.action == Msg.claim) {
      final orderId = optInt(a.data['order_id']);
      if (a.ok) {
        if (orderId != null) {
          // 成功文案和音效都交给状态机，保证同一单只响一次；事件那边
          // (`order_claimed`) 已经写过一行"抢到了"，这里不再重复。
          _resolveClaimSuccess(orderId);
        } else {
          note('抢单成功', kind: LogKind.good);
        }
        return;
      }
      // 服务端的 message 比通用文案更具体（"已被其他骑手抢走"这种）。
      final said = a.message;
      final msg =
          (said != null && said.isNotEmpty) ? said : ErrCode.humanize(a.code);
      lastError = msg;
      errorSeq++;
      note(msg, kind: LogKind.bad);
      // 没有 order_id 的是旧服务端，只能退回全局提示，卡片无法精确定位。
      if (orderId != null) _failClaim(orderId, a.code, message: msg);
      return;
    }
    if (a.ok) {
      final label = switch (a.action) {
        Msg.claim => '抢单成功',
        Msg.deliver => '已送达',
        Msg.arriveShop => '已到店',
        Msg.arriveCustomer => '已到客户',
        Msg.reject => '已拒单',
        Msg.acceptAssign => '已接下指派单',
        Msg.urge => '已催单',
        Msg.setOnline => '在线状态已更新',
        Msg.setSpeedMode => '骑行模式已更新',
        Msg.changePassword => '密码已改，二维码里的旧密码作废',
        Msg.taskMessage => '消息已发出，等顾客回',
        Msg.chatSay => '消息已发出',
        Msg.chatPreset => '话术已发送',
        _ => '${a.action} 成功',
      };
      note(label, kind: LogKind.good);
      return;
    }
    // 失败也要走「当场提示」通道：只写进通知列表，骑手按完按钮没反应会更懵。
    // 到店 / 送达被拒、取送失败都在这里，首页靠 `errorSeq` 去重弹 toast。
    final msg = ErrCode.humanize(a.code);
    lastError = msg;
    errorSeq++;
    note(msg, kind: LogKind.bad);
  }

  /// 时钟类事件：把插值基准往前推。
  void _touch(Map<String, dynamic> p) {
    final m = optInt(p['elapsed_min']);
    if (m != null) {
      _syncedMinute = m;
      _syncedAt = DateTime.now();
      elapsedMin = m;
    }
    final sm = optInt(p['sim_minute']);
    if (sm != null) simMinute = sm;
    final d = optInt(p['day']);
    if (d != null) day = d;
  }

  // ---------------------------------------------------------------------
  // 订单
  // ---------------------------------------------------------------------

  void _applyOrderBroadcast(Map<String, dynamic> p) {
    final order = OrderView.fromJson(asMap(p['order']));
    _upsertOrder(order);
    note(
      '新单：${order.merchantName} → ${order.distanceLabel}，'
      '到手 ${yuan(order.effectiveTakehomeCents)} 元',
      kind: LogKind.money,
    );
  }

  void _applyOrderClaimed(Map<String, dynamic> p) {
    final id = reqInt(p['order_id']);
    final by = optInt(p['rider_id']);
    final mine = by != null && by == riderId;
    final forced = reqBool(p['forced']);
    // 旧服务端没有这个字段：那时平台塞过来的单都是可拒的，按指派单处理。
    final mode = p['assign_mode'] is String ? p['assign_mode'] as String : null;
    final idx = hall.indexWhere((o) => o.id == id);
    final attempt = claims[id];
    if (idx >= 0) {
      final old = hall[idx];
      if (attempt != null && mine) {
        // 自己刚抢到的单：大厅这张卡先留在原位显示 1.5s 成功态。
        // 下面照样进 activeOrders，所以待取数量和收益立刻就是对的。
        hall[idx] =
            _copyOrder(old, status: OrderStatus.assigned, assignedTo: by);
      } else {
        hall.removeAt(idx);
      }
      if (!activeOrders.any((o) => o.id == id)) {
        activeOrders.add(
          _copyOrder(old, status: OrderStatus.assigned, assignedTo: by),
        );
      }
    }
    if (attempt != null) {
      if (mine) {
        // 事件里的 note 已经写了"抢到了"，这里只画成功态、响一声。
        _resolveClaimSuccess(id, announce: false);
      } else if (attempt.active) {
        _failClaim(id, ErrCode.alreadyClaimed);
      }
    }
    // 平台派来的单要骑手当面确认，先弹界面再算数：
    // 指派单可以拒（吃额度），强制派单只能同意。
    if (mine && (mode != null || forced)) {
      assignOrderId = id;
      _assignMode = mode ?? 'offer';
      _assignAt = DateTime.now();
      _alertAssign(id, forced: forced);
    }
    note(
      mine
          ? switch (mode) {
              'forced' => '强制派单：${reqStr(p['rider_name'], '你')} 塞来一单，只能同意',
              'offer' => '指派单：${reqStr(p['rider_name'], '你')} 给你派了一单',
              _ =>
                '抢到了：${reqStr(p['rider_name'], '你')} → ${yuan(reqInt(p['quoted_fee_cents']))} 元',
            }
          : '单 ${reqStr(p['rider_name'], '他人')} 抢走了',
      kind: mine ? LogKind.good : LogKind.info,
    );
  }

  /// 指派单 / 强制派单的提醒。
  ///
  /// 正文要包含骑手**决定要不要接**所需的三样东西：哪儿、多远、多少钱。
  /// 窗口 30 秒（[assignWindow]），锁屏上看到就得能判断。
  void _alertAssign(int id, {required bool forced}) {
    final order = orderById(id);
    if (order == null) return;
    onAlert?.call(
      RiderAlert(
        kind: forced ? AlertKind.forcedAssign : AlertKind.assignOrder,
        title: forced ? '强制派单（只能接受）' : '新的指派单',
        body: '${order.merchantName} · ${order.distanceLabel} · '
            '到手 ${yuan(order.effectiveTakehomeCents)} 元 · '
            '${assignWindow.inSeconds} 秒内确认',
      ),
    );
  }

  /// 新上架的活动提醒。
  ///
  /// 只比 id：烽火台改标题 / 下架重上也算同一条，不重复扰人。
  /// 连接时的快照不走这里（见 [applyFrame] 的 snapshot 分支），
  /// 否则每次断线重连都会把现有活动再提醒一遍。
  void _announceNewActivities(Set<int> before) {
    final fresh =
        activities.where((a) => a.active && !before.contains(a.id)).toList();
    if (fresh.isEmpty) return;
    final first = fresh.first;
    final tail = fresh.length == 1 ? '' : ' 等 ${fresh.length} 个活动';
    onAlert?.call(
      RiderAlert(
        kind: AlertKind.activity,
        title: '新平台活动',
        body: '${first.title}$tail：跑 ${first.targetOrders} 单，'
            '奖 ${yuan(first.rewardCents)} 元',
      ),
    );
  }

  /// 拒掉了一张还没确认的指派单：单退回大厅，额度 +1。
  void _applyAssignRejected(Map<String, dynamic> p) {
    final id = reqInt(p['order_id']);
    final used = reqInt(p['used']);
    final limit = reqInt(p['limit']);
    final forcedOffline = reqBool(p['forced_offline']);
    if (assignOrderId == id) resolveAssign();
    note(
      limit > 0 ? '拒单已记入额度：今日 $used/$limit' : '拒单已记入额度：今日 $used（不限）',
      kind: LogKind.info,
    );
    // 每一次拒单都要让骑手看见「现在拒了几次」——这是额度的唯一反馈，
    // 藏在通知列表里等于没有。用序号驱动首页弹窗，重放旧事件不会重复弹。
    rejectNoticeUsed = used;
    rejectNoticeLimit = limit;
    rejectNoticeForcedOffline = forcedOffline;
    rejectNoticeSeq++;
    if (forcedOffline) {
      note('拒单额度用尽，你已被强制下线', kind: LogKind.bad);
    }
  }

  void _applyOrderPickedUp(Map<String, dynamic> p) {
    final id = reqInt(p['order_id']);
    _patchOrder(id, (o) => _copyOrder(o, status: OrderStatus.pickedUp));
    note('已取货（出餐 ${reqInt(p['prep_minutes'])} 分钟）');
  }

  void _applyOrderDelivered(Map<String, dynamic> p) {
    final id = reqInt(p['order_id']);
    final mine = optInt(p['rider_id']) == riderId;
    _patchOrder(
      id,
      (o) => _copyOrder(
        o,
        status: reqBool(p['late']) ? OrderStatus.late : OrderStatus.delivered,
      ),
    );
    activeOrders.removeWhere((o) => o.id == id);
    if (mine) {
      note(
        '送达！到手 ${yuan(reqInt(p['takehome_cents']))} 元，'
        '时薪 ${yuan(reqInt(p['hourly_wage_cents']))} 元',
        kind: LogKind.money,
      );
    }
  }

  void _upsertOrder(OrderView o) {
    _patchOrder(o.id, (_) => o);
    if (o.status == OrderStatus.pending) {
      if (!hall.any((x) => x.id == o.id)) hall.add(o);
    } else if (o.status.isActive) {
      if (!activeOrders.any((x) => x.id == o.id)) activeOrders.add(o);
    }
  }

  void _patchOrder(int id, OrderView Function(OrderView) update) {
    final hi = hall.indexWhere((o) => o.id == id);
    if (hi >= 0) hall[hi] = update(hall[hi]);
    final ai = activeOrders.indexWhere((o) => o.id == id);
    if (ai >= 0) activeOrders[ai] = update(activeOrders[ai]);
  }

  void _removeOrder(int id) {
    hall.removeWhere((o) => o.id == id);
    activeOrders.removeWhere((o) => o.id == id);
    _claimTimers.remove(id)?.cancel();
    claims.remove(id);
    _celebrated.remove(id);
    if (assignOrderId == id) resolveAssign();
  }

  // ---------------------------------------------------------------------
  // 骑手 / 财务
  // ---------------------------------------------------------------------

  /// 自己上线 / 下线时响一声。
  ///
  /// 以服务端回来的 `online` 为准，而不是按钮按下的那一刻——被强制下线、
  /// 拒单额度用尽这类情况服务端会改状态，跟着它走才不会报错消息。
  void _watchOnline() {
    final now = me?.online;
    if (now == null) return;
    final before = _lastOnline;
    _lastOnline = now;
    if (before == null || before == now) return;
    onAlert?.call(
      RiderAlert(
        kind: now ? AlertKind.online : AlertKind.offline,
        title: now ? '已上线' : '已下线',
        body: now ? '开始接单' : '注意休息',
      ),
    );
  }

  void _applyRiderState(Map<String, dynamic> p) {
    final r = RiderView.fromJson(asMap(p['rider']));
    riders[r.id] = r;
  }

  void _applyFinance(Map<String, dynamic> p) {
    final delta = reqInt(p['delta_cents']);
    if (optInt(p['rider_id']) != riderId) return;
    final reason = reqStr(p['reason'], reqStr(p['kind']));
    walletEntries.insert(
      0,
      WalletEntry(
        at: DateTime.now(),
        title: reason,
        cents: delta,
        kind: reqStr(p['kind'], 'other'),
      ),
    );
    if (walletEntries.length > 300) walletEntries.removeLast();
    note(
      '${delta >= 0 ? '+' : ''}${yuan(delta)} 元：$reason',
      kind: LogKind.money,
    );
    // 财务字段会随后续的 rider_state_changed 修正；本地先乐观累加。
    _patchMe((r) => _copyRider(r,
        takehomeCents:
            r.takehomeCents == null ? null : r.takehomeCents! + delta));
  }

  void _patchMe(RiderView Function(RiderView) update) {
    final id = riderId;
    if (id == null) return;
    final cur = riders[id];
    if (cur != null) riders[id] = update(cur);
  }

  /// 把「我」的位置挪到手机刚拿到的定位处（世界坐标 GCJ-02）。
  ///
  /// 服务端的 tick 帧**不带骑手坐标**，只有 snapshot 才带——不在这里补一下，
  /// 地图上的「我」和订单距离会一直停在连接那一刻（这就是「定位不刷新」）。
  /// 先动本地，下一次 snapshot 会用同一个值覆盖回来，不会来回跳。
  void applyLocalFix(double lat, double lng) {
    _patchMe((r) => _copyRider(r, lat: lat, lng: lng));
    notifyListeners();
  }

  void _applyWeather(Map<String, dynamic> p) {
    weather = reqStr(p['weather'], weather);
    weatherLabel = reqStr(p['label'], weatherLabel);
    weatherSpeedFactor = reqDouble(p['speed_factor'], 1);
    final bonus = reqInt(p['rain_bonus_cents']);
    note(
      '天气转为$weatherLabel${bonus > 0 ? '，雨补 ${yuan(bonus)} 元' : ''}',
      kind: LogKind.info,
    );
  }

  void _applyConfig(Map<String, dynamic> p) {
    note('世界配置变化：${reqInt(p['speed_multiplier'], 1)}x', kind: LogKind.info);
  }

  void _applyAiDecision(Map<String, dynamic> p) {
    final n =
        (p['assignments'] is List) ? (p['assignments'] as List).length : 0;
    if (n > 0) note('AI 指派了 $n 单', kind: LogKind.info);
  }

  void _applyTaskMessage(Map<String, dynamic> p) {
    final id = reqInt(p['order_id']);
    // 任务消息服务端不带时间，收到时用插值后的时钟补一个，才能和末端会话混排。
    final minute = nowMinute;
    final hh = (minute % 1440 ~/ 60).toString().padLeft(2, '0');
    final mm = (minute % 60).toString().padLeft(2, '0');
    final line = TaskLine(
      from: reqStr(p['from'], 'customer'),
      text: reqStr(p['text']),
      step: reqInt(p['step']),
      at: '$hh:$mm',
      atMin: minute,
    );
    taskLines.putIfAbsent(id, () => []).add(line);
    _timelines.putIfAbsent(id, () => []).add(ChatEntry.ofTask(line));
    note(
      '${line.isMe ? '我' : '顾客'}：${line.text}',
      kind: LogKind.chat,
    );
  }

  /// 末端会话消息。
  ///
  /// 与 [TaskLine] 那条路分开：这里不推进任何任务进度，纯粹是末端沟通。
  /// **带赔付的消息要显眼**——骑手得立刻知道这一单被扣了钱。
  void _applyChatMessage(Map<String, dynamic> p) {
    final id = reqInt(p['order_id']);
    final line = ChatLine.fromJson(p);
    chatLines.putIfAbsent(id, () => []).add(line);
    _timelines.putIfAbsent(id, () => []).add(ChatEntry.ofChat(line));

    final who = switch (line.from) {
      'rider' => '我',
      'customer' => '顾客',
      _ => '系统',
    };
    if (line.isCostly) {
      note(
        '$who：${line.text}（赔付 ${yuan(line.compensationCents)} 元）',
        kind: LogKind.money,
      );
    } else {
      note('$who：${line.text}', kind: LogKind.chat);
    }
  }

  void _applyDayClosed(Map<String, dynamic> p) {
    ledgers = listOf(p['ledgers'], DayLedgerView.fromJson);
    // 跨天了，上一轮的成功/失败卡片与去重集合都不该留到新的一天。
    for (final t in _claimTimers.values) {
      t.cancel();
    }
    _claimTimers.clear();
    claims.clear();
    _celebrated.clear();
    note(
      '第 ${reqInt(p['day'])} 天结算，总到手 ${yuan(reqInt(p['total_takehome_cents']))} 元',
      kind: LogKind.money,
    );
  }

  // ---------------------------------------------------------------------
  // 不可变拷贝（模型没有 copyWith，这里集中补）
  // ---------------------------------------------------------------------

  static OrderView _copyOrder(
    OrderView o, {
    OrderStatus? status,
    int? assignedTo,
  }) =>
      OrderView(
        id: o.id,
        merchantName: o.merchantName,
        merchantLat: o.merchantLat,
        merchantLng: o.merchantLng,
        customerName: o.customerName,
        customerAddress: o.customerAddress,
        remark: o.remark,
        customerLat: o.customerLat,
        customerLng: o.customerLng,
        distanceM: o.distanceM,
        promisedMinutes: o.promisedMinutes,
        tier: o.tier,
        createdAtMin: o.createdAtMin,
        claimDeadlineMin: o.claimDeadlineMin,
        pickedUpAtMin: o.pickedUpAtMin,
        deliveredAtMin: o.deliveredAtMin,
        fee: o.fee,
        redispatchCount: o.redispatchCount,
        surchargeCents: o.surchargeCents,
        status: status ?? o.status,
        assignedTo: assignedTo ?? o.assignedTo,
        forced: o.forced,
        assignMode: o.assignMode,
        backOrder: o.backOrder,
        urgeCount: o.urgeCount,
        prepMinutes: o.prepMinutes,
        task: o.task,
      );

  static RiderView _copyRider(
    RiderView r, {
    int? takehomeCents,
    double? lat,
    double? lng,
    List<ActivityProgressView>? activities,
  }) =>
      RiderView(
        id: r.id,
        key: r.key,
        name: r.name,
        kind: r.kind,
        online: r.online,
        injured: r.injured,
        frozen: r.frozen,
        speedMode: r.speedMode,
        level: r.level,
        rating: r.rating,
        completed: r.completed,
        rejectsToday: r.rejectsToday,
        forcedOffline: r.forcedOffline,
        held: r.held,
        heldCap: r.heldCap,
        phase: r.phase,
        lat: lat ?? r.lat,
        lng: lng ?? r.lng,
        hourlyWageCents: r.hourlyWageCents,
        takehomeCents: takehomeCents ?? r.takehomeCents,
        cashCents: r.cashCents,
        finesCents: r.finesCents,
        activities: activities ?? r.activities,
      );
}

import 'package:flutter/foundation.dart';

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

/// 客户端世界状态。
///
/// **这是 UI 的唯一数据源**：网络层只负责把帧喂给 [applyFrame]，
/// UI 只监听本对象。所有解析失败都在这里被吞掉并记一条日志，绝不崩界面。
class GameState extends ChangeNotifier {
  // ---------------------------------------------------------------------
  // 连接 / 身份
  // ---------------------------------------------------------------------
  ConnStatus status = ConnStatus.idle;
  String serverUrl = kDefaultServerUrl;
  String role = 'spectator';
  int? riderId;
  String? riderKey;

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

  /// 每条订单的任务对话（绑在附加任务上）。
  final Map<int, List<TaskLine>> taskLines = {};

  /// 每条订单的**末端会话**（末端交付沟通，与附加任务无关）。
  ///
  /// 服务端那边时效一天，日结时清库；本地缓存跟着在 `day_closed` 时清空，
  /// 见 [applyFrame]。
  final Map<int, List<ChatLine>> chatLines = {};

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
    final drift = DateTime.now().difference(_syncedAt).inMilliseconds ~/ interval;
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
      (o.claimDeadlineMin - nowMinute) * (tickIntervalMs <= 0 ? 1000 : tickIntervalMs);

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
  DateTime? _assignAt;

  /// 确认窗口。玩法设计 §4.3：30 秒内不确认则自动拒绝。
  ///
  /// 按**真实秒**（挂钟）计，不随模拟时间倍率缩放——这是留给玩家反应的窗口。
  static const assignWindow = Duration(seconds: 30);

  /// 指派单还能犹豫多少毫秒。
  int get assignRemainingMs {
    final at = _assignAt;
    if (at == null) return 0;
    final left =
        assignWindow.inMilliseconds - DateTime.now().difference(at).inMilliseconds;
    return left < 0 ? 0 : left;
  }

  /// 待确认的指派单本体（可能还没同步到，见 [assignOrderId] 的兜底逻辑）。
  OrderView? get assignOrder {
    final id = assignOrderId;
    return id == null ? null : orderById(id);
  }

  /// 关闭指派单确认界面（接受或拒绝之后都要调）。
  void resolveAssign() {
    if (assignOrderId == null && _assignAt == null) return;
    assignOrderId = null;
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
      default:
        break; // 未知事件：忽略，向前兼容
    }
    notifyListeners();
  }

  // ---------------------------------------------------------------------
  // 传输层
  // ---------------------------------------------------------------------

  void _applyWelcome(Welcome w) {
    role = w.role;
    riderId = w.riderId;
    riderKey = w.riderKey;
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
    if (announce) note('已同步世界状态');
  }

  void _applyAck(Ack a) {
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
    note(ErrCode.humanize(a.code), kind: LogKind.bad);
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
    final idx = hall.indexWhere((o) => o.id == id);
    if (idx >= 0) {
      final old = hall.removeAt(idx);
      activeOrders.add(_copyOrder(old, status: OrderStatus.assigned, assignedTo: by));
    }
    // 系统塞的单要骑手当面确认，先弹界面再算数。
    //
    // 额度已用尽时也照弹——那正是要让骑手看见「你今天拒绝已超出上限」小窗的
    // 时刻；只是确认界面里「拒绝」会被禁掉，只剩接受（见 AssignOrderPage）。
    if (mine && forced) {
      assignOrderId = id;
      _assignAt = DateTime.now();
    }
    note(
      mine
          ? (forced ? '平台指派：${reqStr(p['rider_name'], '你')} 塞来一单' : '抢到了：${reqStr(p['rider_name'], '你')} → ${yuan(reqInt(p['quoted_fee_cents']))} 元')
          : '单 ${reqStr(p['rider_name'], '他人')} 抢走了',
      kind: mine ? LogKind.good : LogKind.info,
    );
  }

  /// 拒掉了一张还没确认的指派单：单退回大厅，额度 +1。
  void _applyAssignRejected(Map<String, dynamic> p) {
    final id = reqInt(p['order_id']);
    final used = reqInt(p['used']);
    final limit = reqInt(p['limit']);
    if (assignOrderId == id) resolveAssign();
    note(
      limit > 0 ? '拒单已记入额度：今日 $used/$limit' : '拒单已记入额度：今日 $used（不限）',
      kind: LogKind.info,
    );
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
    if (assignOrderId == id) resolveAssign();
  }

  // ---------------------------------------------------------------------
  // 骑手 / 财务
  // ---------------------------------------------------------------------

  void _applyRiderState(Map<String, dynamic> p) {
    final r = RiderView.fromJson(asMap(p['rider']));
    riders[r.id] = r;
  }

  void _applyFinance(Map<String, dynamic> p) {
    final delta = reqInt(p['delta_cents']);
    if (optInt(p['rider_id']) != riderId) return;
    note(
      '${delta >= 0 ? '+' : ''}${yuan(delta)} 元：${reqStr(p['reason'], reqStr(p['kind']))}',
      kind: LogKind.money,
    );
    // 财务字段会随后续的 rider_state_changed 修正；本地先乐观累加。
    _patchMe((r) => _copyRider(r,
        takehomeCents: r.takehomeCents == null ? null : r.takehomeCents! + delta));
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
    final n = (p['assignments'] is List) ? (p['assignments'] as List).length : 0;
    if (n > 0) note('AI 指派了 $n 单', kind: LogKind.info);
  }

  void _applyTaskMessage(Map<String, dynamic> p) {
    final id = reqInt(p['order_id']);
    final line = TaskLine(
      from: reqStr(p['from'], 'customer'),
      text: reqStr(p['text']),
      step: reqInt(p['step']),
    );
    taskLines.putIfAbsent(id, () => []).add(line);
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
        held: r.held,
        phase: r.phase,
        lat: lat ?? r.lat,
        lng: lng ?? r.lng,
        hourlyWageCents: r.hourlyWageCents,
        takehomeCents: takehomeCents ?? r.takehomeCents,
        cashCents: r.cashCents,
        finesCents: r.finesCents,
      );
}

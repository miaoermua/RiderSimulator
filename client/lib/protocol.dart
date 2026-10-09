/// 骑手端协议层。
///
/// 只做两件事：**定义固定字符串**、**把 JSON 解析成模型**。
/// 不改状态、不碰网络——这样它可以被单独单测。
///
/// 契约来源：`packages/protocol.json`。
/// 服务端权威实现：`server/src/net/protocol.rs`。
library;

/// 协议版本。与服务端不一致会被拒绝握手。
const String kProtocolVersion = '1.0';

/// 默认服务端地址。
const String kDefaultServerUrl = 'ws://127.0.0.1:8080/ws';

/// 上行消息类型。
abstract final class Msg {
  static const hello = 'hello';
  static const resync = 'resync';
  static const ping = 'ping';
  static const claim = 'claim';
  static const reject = 'reject';
  static const urge = 'urge';
  static const arriveShop = 'arrive_shop';
  static const arriveCustomer = 'arrive_customer';
  static const deliver = 'deliver';
  static const setOnline = 'set_online';
  static const setSpeedMode = 'set_speed_mode';
  static const taskMessage = 'task_message';
}

/// 下行事件名。前缀为传输层事件，其余直接来自服务端模拟事件。
abstract final class Ev {
  // 传输层
  static const welcome = 'welcome';
  static const snapshot = 'snapshot';
  static const ack = 'ack';
  static const error = 'error';
  static const pong = 'pong';
  static const serverInfo = 'server_info';

  // 模拟事件
  static const tick = 'tick';
  static const orderBroadcast = 'order_broadcast';
  static const orderClaimed = 'order_claimed';
  static const orderExpired = 'order_expired';
  static const orderPickedUp = 'order_picked_up';
  static const orderDelivered = 'order_delivered';
  static const riderStateChanged = 'rider_state_changed';
  static const riderFinance = 'rider_finance';
  static const weatherChanged = 'weather_changed';
  static const fineIssued = 'fine_issued';
  static const configChanged = 'config_changed';
  static const aiDecision = 'ai_decision';
  static const taskMessage = 'task_message';
  static const dayClosed = 'day_closed';
}

/// 错误码。**一律按 code 分支，不要匹配中文文案。**
abstract final class ErrCode {
  static const badRequest = 'BAD_REQUEST';
  static const frameTooLarge = 'FRAME_TOO_LARGE';
  static const helloRequired = 'HELLO_REQUIRED';
  static const protocolMismatch = 'PROTOCOL_MISMATCH';
  static const unauthorized = 'UNAUTHORIZED';
  static const notARider = 'NOT_A_RIDER';
  static const notAController = 'NOT_A_CONTROLLER';
  static const featureDisabled = 'FEATURE_DISABLED';
  static const noTask = 'NO_TASK';
  static const taskFinished = 'TASK_FINISHED';
  static const internal = 'INTERNAL';

  static const orderNotFound = 'ORDER_NOT_FOUND';
  static const riderNotFound = 'RIDER_NOT_FOUND';
  static const alreadyClaimed = 'ALREADY_CLAIMED';
  static const orderExpired = 'ORDER_EXPIRED';
  static const riderOffline = 'RIDER_OFFLINE';
  static const riderFrozen = 'RIDER_FROZEN';
  static const riderInjured = 'RIDER_INJURED';
  static const ratingTooLow = 'RATING_TOO_LOW';
  static const heldLimitReached = 'HELD_LIMIT_REACHED';
  static const backOrderDisabled = 'BACK_ORDER_DISABLED';
  static const backOrderDetourTooLong = 'BACK_ORDER_DETOUR_TOO_LONG';
  static const wrongPhase = 'WRONG_PHASE';
  static const notAssignedToYou = 'NOT_ASSIGNED_TO_YOU';

  /// 给用户看的短提示。未知码原样返回，方便发现契约漂移。
  static String humanize(String? code) => switch (code) {
        null || '' => '未知错误',
        alreadyClaimed => '手慢了，单被抢走了',
        orderExpired => '这单已经过期',
        ratingTooLow => '评分不够，抢不了这一档',
        heldLimitReached => '手上的单太多了',
        riderOffline => '你先上线才能接单',
        riderFrozen => '账号被冻结了',
        riderInjured => '你受伤了，先歇会儿',
        wrongPhase => '当前状态不允许这个操作',
        notAssignedToYou => '这不是你的订单',
        noTask => '这一单没有附加任务',
        taskFinished => '任务已经做完了',
        notARider => '观战者不能跑单',
        unauthorized => '令牌无效',
        protocolMismatch => '客户端版本与服务端不一致',
        _ => code ?? '未知错误',
      };
}

/// 服务端下行的一帧：`{ "event": ..., "payload": ... }`。
class ServerFrame {
  const ServerFrame(this.event, this.payload);

  final String event;
  final Map<String, dynamic> payload;
}

/// 从原始 JSON 解析一帧。无法识别时返回 `null`（绝不抛）。
ServerFrame? parseFrame(Object? raw) {
  if (raw is! Map) return null;
  final event = raw['event'];
  if (event is! String || event.isEmpty) return null;
  final payload = raw['payload'];
  return ServerFrame(
    event,
    payload is Map ? Map<String, dynamic>.from(payload) : const {},
  );
}

// ---------------------------------------------------------------------------
// 安全取值：服务端字段可能因为角色被裁剪成 null，这里统一兜底。
// ---------------------------------------------------------------------------

int? optInt(Object? v) => v is num ? v.toInt() : null;
int reqInt(Object? v, [int fallback = 0]) => optInt(v) ?? fallback;
double optDouble(Object? v) => v is num ? v.toDouble() : null;
double reqDouble(Object? v, [double fallback = 0]) => optDouble(v) ?? fallback;
bool reqBool(Object? v, [bool fallback = false]) => v is bool ? v : fallback;
String reqStr(Object? v, [String fallback = '']) => v is String ? v : fallback;

Map<String, dynamic> asMap(Object? v) =>
    v is Map ? Map<String, dynamic>.from(v) : const {};

List<T> listOf<T>(Object? v, T Function(Map<String, dynamic>) make) => v is List
    ? v
        .whereType<Map>()
        .map((e) => make(Map<String, dynamic>.from(e)))
        .toList()
    : const [];

/// 分 → 元，保留两位。
String yuan(int cents) {
  final sign = cents < 0 ? '-' : '';
  final abs = cents.abs();
  return '$sign${abs ~/ 100}.${(abs % 100).toString().padLeft(2, '0')}';
}

/// 分钟数 → `HH:MM`（当日 0:00 起算）。
String clockLabel(int minuteOfDay) {
  final m = minuteOfDay % 1440;
  return '${(m ~/ 60).toString().padLeft(2, '0')}:'
      '${(m % 60).toString().padLeft(2, '0')}';
}

// ---------------------------------------------------------------------------
// 模型
// ---------------------------------------------------------------------------

enum OrderStatus {
  pending,
  assigned,
  pickedUp,
  delivered,
  late,
  expired,
  cancelled;

  static OrderStatus parse(String raw) => switch (raw) {
        'pending' => pending,
        'assigned' => assigned,
        'picked_up' => pickedUp,
        'delivered' => delivered,
        'late' => late,
        'expired' => expired,
        'cancelled' => cancelled,
        _ => cancelled,
      };

  String get label => switch (this) {
        pending => '待抢',
        assigned => '已接单',
        pickedUp => '配送中',
        delivered => '已送达',
        late => '超时送达',
        expired => '已过期',
        cancelled => '已取消',
      };

  bool get isActive => this == pending || this == assigned || this == pickedUp;
}

enum OrderTier {
  normal,
  premium,
  express;

  static OrderTier parse(String raw) => switch (raw) {
        'premium' => premium,
        'express' => express,
        _ => normal,
      };

  String get label => switch (this) {
        normal => '普通',
        premium => '优质',
        express => '急单',
      };
}

enum RiderPhase {
  toShop,
  toCustomer;

  static RiderPhase? parse(Object? raw) => switch (raw) {
        'to_shop' => toShop,
        'to_customer' => toCustomer,
        _ => null,
      };

  String get label => this == toShop ? '去商家' : '去客户';
}

enum RiderKind {
  simulated,
  human;

  static RiderKind parse(String raw) => raw == 'human' ? human : simulated;

  String get label => this == human ? '真人' : 'AI';
}

enum SpeedMode {
  compliant,
  tolerant,
  unrestricted;

  static SpeedMode parse(String raw) => switch (raw) {
        'unrestricted' => unrestricted,
        'tolerant' => tolerant,
        _ => compliant,
      };

  String get label => switch (this) {
        compliant => '守规',
        tolerant => '轻微超速',
        unrestricted => '不限速',
      };

  /// 上行用的字符串。
  String get wire => switch (this) {
        compliant => 'compliant',
        tolerant => 'tolerant',
        unrestricted => 'unrestricted',
      };
}

class FeeView {
  const FeeView({
    required this.baseFeeCents,
    required this.distanceExtraCents,
    required this.rainBonusCents,
    required this.levelBonusCents,
    required this.surgeMultiplier,
    required this.commissionCents,
    required this.quotedFeeCents,
    required this.takehomeCents,
  });

  final int baseFeeCents;
  final int distanceExtraCents;
  final int rainBonusCents;
  final int levelBonusCents;
  final double surgeMultiplier;
  final int commissionCents;
  final int quotedFeeCents;
  final int takehomeCents;

  factory FeeView.fromJson(Map<String, dynamic> j) => FeeView(
        baseFeeCents: reqInt(j['base_fee_cents']),
        distanceExtraCents: reqInt(j['distance_extra_cents']),
        rainBonusCents: reqInt(j['rain_bonus_cents']),
        levelBonusCents: reqInt(j['level_bonus_cents']),
        surgeMultiplier: reqDouble(j['surge_multiplier'], 1),
        commissionCents: reqInt(j['commission_cents']),
        quotedFeeCents: reqInt(j['quoted_fee_cents']),
        takehomeCents: reqInt(j['takehome_cents']),
      );
}

/// 里程阶梯的一档。
class ConfigDistanceTier {
  const ConfigDistanceTier({required this.fromM, required this.cents});

  /// 距离下界（米）。本单距离取不超过它的最大档。
  final int fromM;
  /// 该档的总附加费（分）。
  final int cents;
}

/// 计价规则（服务端 `FeeView`）。
///
/// 与订单上的 [FeeView] 不是一回事：那个是**某一单**算出来的钱，
/// 这个是**规则本身**，控制台「计价」面板据此渲染并调整。
class ConfigFeeRules {
  const ConfigFeeRules({
    required this.baseFeeCents,
    required this.distanceTiers,
    required this.perKmOverCents,
    required this.levelBonusPerLevelCents,
    required this.surgeMultiplier,
    required this.maxRedispatchAttempts,
    required this.redispatchSurchargeStepCents,
  });

  /// 起步价（分），含第一档里程。
  final int baseFeeCents;
  final List<ConfigDistanceTier> distanceTiers;
  /// 超出最后一档后每公里单价（分/km）。
  final int perKmOverCents;
  /// 等级加成（分/级）。
  final int levelBonusPerLevelCents;
  final double surgeMultiplier;
  /// 订单重投上限与每次加价。
  final int maxRedispatchAttempts;
  final int redispatchSurchargeStepCents;

  factory ConfigFeeRules.fromJson(Map<String, dynamic> j) => ConfigFeeRules(
        baseFeeCents: reqInt(j['base_fee_cents']),
        distanceTiers: _tiers(j['distance_tiers']),
        perKmOverCents: reqInt(j['per_km_over_cents']),
        levelBonusPerLevelCents: reqInt(j['level_bonus_per_level_cents']),
        surgeMultiplier: reqDouble(j['surge_multiplier'], 1),
        maxRedispatchAttempts: reqInt(j['max_redispatch_attempts']),
        redispatchSurchargeStepCents: reqInt(j['redispatch_surcharge_step_cents']),
      );

  /// 阶梯是 `[[距离, 金额]]` 这样的**数组对**，不是对象数组，
  /// 所以走不了 `listOf`（它只收 Map）。
  static List<ConfigDistanceTier> _tiers(Object? v) {
    if (v is! List) return const [];
    return v.whereType<List>().map((e) {
      return ConfigDistanceTier(
        fromM: reqInt(e.isNotEmpty ? e[0] : null),
        cents: reqInt(e.length > 1 ? e[1] : null),
      );
    }).toList();
  }
}

class ExtraTaskView {
  const ExtraTaskView({
    required this.id,
    required this.name,
    required this.step,
    required this.done,
    required this.failed,
    required this.rewardCents,
    required this.penaltyCents,
  });

  final String id;
  final String name;
  final int step;
  final bool done;
  final bool failed;
  final int rewardCents;
  final int penaltyCents;

  factory ExtraTaskView.fromJson(Map<String, dynamic> j) => ExtraTaskView(
        id: reqStr(j['id']),
        name: reqStr(j['name']),
        step: reqInt(j['step']),
        done: reqBool(j['done']),
        failed: reqBool(j['failed']),
        rewardCents: reqInt(j['reward_cents']),
        penaltyCents: reqInt(j['penalty_cents']),
      );
}

class OrderView {
  const OrderView({
    required this.id,
    required this.merchantName,
    required this.merchantLat,
    required this.merchantLng,
    required this.customerName,
    required this.customerAddress,
    required this.remark,
    required this.customerLat,
    required this.customerLng,
    required this.distanceM,
    required this.promisedMinutes,
    required this.tier,
    required this.createdAtMin,
    required this.claimDeadlineMin,
    required this.pickedUpAtMin,
    required this.deliveredAtMin,
    required this.fee,
    required this.redispatchCount,
    required this.surchargeCents,
    required this.status,
    required this.assignedTo,
    required this.forced,
    required this.backOrder,
    required this.urgeCount,
    required this.prepMinutes,
    required this.task,
  });

  final int id;
  final String merchantName;
  final double merchantLat;
  final double merchantLng;
  final String customerName;
  final String customerAddress;
  final String remark;
  final double customerLat;
  final double customerLng;
  final int distanceM;
  final int promisedMinutes;
  final OrderTier tier;
  final int createdAtMin;
  final int claimDeadlineMin;
  final int? pickedUpAtMin;
  final int? deliveredAtMin;
  final FeeView fee;
  /// 退回大厅重投的次数：没人接、或骑手取消都会累加。
  final int redispatchCount;
  /// 重投叠加的调度补贴（分），全给骑手、不抽水。
  final int surchargeCents;
  final OrderStatus status;
  final int? assignedTo;
  final bool forced;
  final bool backOrder;
  final int urgeCount;
  final int? prepMinutes;
  final ExtraTaskView? task;

  bool isMine(int? riderId) => riderId != null && assignedTo == riderId;

  /// 骑手实际看到的报价 = 计费规则报价 + 重投补贴。
  ///
  /// 直接读 `fee.quotedFeeCents` 会少算一截：订单没人接或被取消后会加价重投，
  /// 那部分补贴单独记在 `surchargeCents` 上，不计入 `fee`。
  int get effectiveQuotedCents => fee.quotedFeeCents + surchargeCents;

  /// 骑手实际到手 = 计费规则到手 + 重投补贴（补贴不抽水）。
  int get effectiveTakehomeCents => fee.takehomeCents + surchargeCents;

  /// 距离展示：`1.2km` / `860m`。
  String get distanceLabel =>
      distanceM >= 1000 ? '${(distanceM / 1000).toStringAsFixed(1)}km' : '${distanceM}m';

  factory OrderView.fromJson(Map<String, dynamic> j) {
    final task = j['task'];
    return OrderView(
      id: reqInt(j['id']),
      merchantName: reqStr(j['merchant_name']),
      merchantLat: reqDouble(j['merchant_lat']),
      merchantLng: reqDouble(j['merchant_lng']),
      customerName: reqStr(j['customer_name']),
      customerAddress: reqStr(j['customer_address']),
      remark: reqStr(j['remark']),
      customerLat: reqDouble(j['customer_lat']),
      customerLng: reqDouble(j['customer_lng']),
      distanceM: reqInt(j['distance_m']),
      promisedMinutes: reqInt(j['promised_minutes']),
      tier: OrderTier.parse(reqStr(j['tier'])),
      createdAtMin: reqInt(j['created_at_min']),
      claimDeadlineMin: reqInt(j['claim_deadline_min']),
      pickedUpAtMin: optInt(j['picked_up_at_min']),
      deliveredAtMin: optInt(j['delivered_at_min']),
      fee: FeeView.fromJson(asMap(j['fee'])),
      redispatchCount: reqInt(j['redispatch_count']),
      surchargeCents: reqInt(j['surcharge_cents']),
      status: OrderStatus.parse(reqStr(j['status'])),
      assignedTo: optInt(j['assigned_to']),
      forced: reqBool(j['forced']),
      backOrder: reqBool(j['back_order']),
      urgeCount: reqInt(j['urge_count']),
      prepMinutes: optInt(j['prep_minutes']),
      task: task is Map ? ExtraTaskView.fromJson(asMap(task)) : null,
    );
  }
}

/// 骑手对外快照。收入字段只有本人 / 控制端才有值。
class RiderView {
  const RiderView({
    required this.id,
    required this.key,
    required this.name,
    required this.kind,
    required this.online,
    required this.injured,
    required this.frozen,
    required this.speedMode,
    required this.level,
    required this.rating,
    required this.completed,
    required this.held,
    required this.phase,
    required this.lat,
    required this.lng,
    required this.hourlyWageCents,
    required this.takehomeCents,
    required this.cashCents,
    required this.finesCents,
  });

  final int id;
  final String key;
  final String name;
  final RiderKind kind;
  final bool online;
  final bool injured;
  final bool frozen;
  final SpeedMode speedMode;
  final int level;
  final double rating;
  final int completed;
  final List<int> held;
  final RiderPhase? phase;
  final double lat;
  final double lng;

  final int? hourlyWageCents;
  final int? takehomeCents;
  final int? cashCents;
  final int? finesCents;

  bool get hasFinance => takehomeCents != null;

  factory RiderView.fromJson(Map<String, dynamic> j) => RiderView(
        id: reqInt(j['id']),
        key: reqStr(j['key']),
        name: reqStr(j['name']),
        kind: RiderKind.parse(reqStr(j['kind'])),
        online: reqBool(j['online']),
        injured: reqBool(j['injured']),
        frozen: reqBool(j['frozen']),
        speedMode: SpeedMode.parse(reqStr(j['speed_mode'])),
        level: reqInt(j['level']),
        rating: reqDouble(j['rating']),
        completed: reqInt(j['completed']),
        held: j['held'] is List
            ? (j['held'] as List).whereType<num>().map((e) => e.toInt()).toList()
            : const [],
        phase: RiderPhase.parse(j['phase']),
        lat: reqDouble(j['lat']),
        lng: reqDouble(j['lng']),
        hourlyWageCents: optInt(j['hourly_wage_cents']),
        takehomeCents: optInt(j['takehome_cents']),
        cashCents: optInt(j['cash_cents']),
        finesCents: optInt(j['fines_cents']),
      );
}

class MerchantView {
  const MerchantView({
    required this.id,
    required this.name,
    required this.address,
    required this.rating,
    required this.signature,
    required this.avgPrepMin,
    required this.lat,
    required this.lng,
  });

  final int id;
  final String name;
  final String address;
  final double rating;
  final String signature;
  final int avgPrepMin;
  final double lat;
  final double lng;

  factory MerchantView.fromJson(Map<String, dynamic> j) => MerchantView(
        id: reqInt(j['id']),
        name: reqStr(j['name']),
        address: reqStr(j['address']),
        rating: reqDouble(j['rating']),
        signature: reqStr(j['signature']),
        avgPrepMin: reqInt(j['avg_prep_min']),
        lat: reqDouble(j['lat']),
        lng: reqDouble(j['lng']),
      );
}

class ConfigView {
  const ConfigView({
    required this.speedMultiplier,
    required this.paused,
    required this.tickIntervalMs,
    required this.commissionRate,
    required this.claimWindowSecs,
    required this.allowSpeeding,
    required this.allowUnrestricted,
    required this.backorderEnabled,
    required this.backorderMax,
    required this.maxOrdersPerDay,
    required this.isPeak,
    required this.surgeMultiplier,
    required this.fee,
    required this.insurancePerDayCents,
  });

  final int speedMultiplier;
  final bool paused;
  final int tickIntervalMs;
  final double commissionRate;
  final int claimWindowSecs;
  final bool allowSpeeding;
  final bool allowUnrestricted;
  final bool backorderEnabled;
  final int backorderMax;
  final int maxOrdersPerDay;
  final bool isPeak;
  final double surgeMultiplier;
  /// 计价规则（单价系统）。过去这里只有一个 `base_fee_cents`，
  /// 起步价以外的里程、等级、重投补贴都拿不到。
  final ConfigFeeRules fee;
  final int insurancePerDayCents;

  factory ConfigView.fromJson(Map<String, dynamic> j) => ConfigView(
        speedMultiplier: reqInt(j['speed_multiplier'], 1),
        paused: reqBool(j['paused']),
        tickIntervalMs: reqInt(j['tick_interval_ms'], 1000),
        commissionRate: reqDouble(j['commission_rate']),
        claimWindowSecs: reqInt(j['claim_window_secs']),
        allowSpeeding: reqBool(j['allow_speeding']),
        allowUnrestricted: reqBool(j['allow_unrestricted']),
        backorderEnabled: reqBool(j['backorder_enabled']),
        backorderMax: reqInt(j['backorder_max']),
        maxOrdersPerDay: reqInt(j['max_orders_per_day']),
        isPeak: reqBool(j['is_peak']),
        surgeMultiplier: reqDouble(j['surge_multiplier'], 1),
        fee: ConfigFeeRules.fromJson(asMap(j['fee'])),
        insurancePerDayCents: reqInt(j['insurance_per_day_cents']),
      );
}

class DayLedgerView {
  const DayLedgerView({
    required this.riderId,
    required this.key,
    required this.name,
    required this.takehomeCents,
    required this.grossCents,
    required this.finesCents,
    required this.completed,
    required this.lateCount,
    required this.onlineMinutes,
    required this.hourlyWageCents,
    required this.rating,
    required this.debtCents,
  });

  final int riderId;
  final String key;
  final String name;
  final int takehomeCents;
  final int grossCents;
  final int finesCents;
  final int completed;
  final int lateCount;
  final int onlineMinutes;
  final int hourlyWageCents;
  final double rating;
  final int debtCents;

  factory DayLedgerView.fromJson(Map<String, dynamic> j) => DayLedgerView(
        riderId: reqInt(j['rider_id']),
        key: reqStr(j['key']),
        name: reqStr(j['name']),
        takehomeCents: reqInt(j['takehome_cents']),
        grossCents: reqInt(j['gross_cents']),
        finesCents: reqInt(j['fines_cents']),
        completed: reqInt(j['completed']),
        lateCount: reqInt(j['late_count']),
        onlineMinutes: reqInt(j['online_minutes']),
        hourlyWageCents: reqInt(j['hourly_wage_cents']),
        rating: reqDouble(j['rating']),
        debtCents: reqInt(j['debt_cents']),
      );
}

/// 断线重连 / 首连时下发的全量快照。
class Snapshot {
  const Snapshot({
    required this.day,
    required this.simMinute,
    required this.elapsedMin,
    required this.weather,
    required this.weatherLabel,
    required this.weatherSpeedFactor,
    required this.rainBonusCents,
    required this.config,
    required this.riders,
    required this.hall,
    required this.activeOrders,
    required this.merchants,
    required this.lastDayLedgers,
    required this.claimWindowMs,
  });

  final int day;
  final int simMinute;
  final int elapsedMin;
  final String weather;
  final String weatherLabel;
  final double weatherSpeedFactor;
  final int rainBonusCents;
  final ConfigView config;
  final List<RiderView> riders;
  final List<OrderView> hall;
  final List<OrderView> activeOrders;
  final List<MerchantView> merchants;
  final List<DayLedgerView> lastDayLedgers;
  final int claimWindowMs;

  factory Snapshot.fromJson(Map<String, dynamic> j) => Snapshot(
        day: reqInt(j['day'], 1),
        simMinute: reqInt(j['sim_minute']),
        elapsedMin: reqInt(j['elapsed_min']),
        weather: reqStr(j['weather']),
        weatherLabel: reqStr(j['weather_label']),
        weatherSpeedFactor: reqDouble(j['weather_speed_factor'], 1),
        rainBonusCents: reqInt(j['rain_bonus_cents']),
        config: ConfigView.fromJson(asMap(j['config'])),
        riders: listOf(j['riders'], RiderView.fromJson),
        hall: listOf(j['hall'], OrderView.fromJson),
        activeOrders: listOf(j['active_orders'], OrderView.fromJson),
        merchants: listOf(j['merchants'], MerchantView.fromJson),
        lastDayLedgers: listOf(j['last_day_ledgers'], DayLedgerView.fromJson),
        claimWindowMs: reqInt(j['claim_window_ms'], 5000),
      );
}

/// 握手成功时下发的 welcome。
class Welcome {
  const Welcome({
    required this.role,
    required this.riderId,
    required this.riderKey,
    required this.tickIntervalMs,
    required this.serverMinute,
    required this.snapshot,
  });

  final String role;
  final int? riderId;
  final String? riderKey;
  final int tickIntervalMs;
  final int serverMinute;
  final Snapshot snapshot;

  factory Welcome.fromJson(Map<String, dynamic> j) => Welcome(
        role: reqStr(j['role'], 'spectator'),
        riderId: optInt(j['rider_id']),
        riderKey: j['rider_key'] is String ? j['rider_key'] as String : null,
        tickIntervalMs: reqInt(j['tick_interval_ms'], 1000),
        serverMinute: reqInt(j['server_minute']),
        snapshot: Snapshot.fromJson(asMap(j['snapshot'])),
      );
}

/// 通用应答。
class Ack {
  const Ack({
    required this.action,
    required this.ok,
    required this.code,
    required this.message,
    required this.data,
  });

  final String action;
  final bool ok;
  final String? code;
  final String? message;
  final Map<String, dynamic> data;

  factory Ack.fromJson(Map<String, dynamic> j) => Ack(
        action: reqStr(j['action']),
        ok: reqBool(j['ok']),
        code: j['code'] is String ? j['code'] as String : null,
        message: j['message'] is String ? j['message'] as String : null,
        data: asMap(j['data']),
      );
}

/// 任务对话里的一句话。
class TaskLine {
  const TaskLine({required this.from, required this.text, required this.step});

  final String from; // rider | customer | system
  final String text;
  final int step;

  bool get isMe => from == 'rider';
}

// 滑动抢单：状态机 + 卡片四态 + 超时对账。
//
// 这里全部用本地帧驱动 `GameState`，不连真实 WebSocket：
// `applyFrame` 就是客户端唯一的入口，接上 Mock 服务器与真机走的是同一条路径。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rider_client/alert_event.dart';
import 'package:rider_client/game_client.dart';
import 'package:rider_client/game_state.dart';
import 'package:rider_client/protocol.dart';
import 'package:rider_client/widgets/order_card.dart';
import 'package:rider_client/widgets/slide_confirm.dart';

const _riderId = 7;

OrderView _order(
  int id, {
  OrderStatus status = OrderStatus.pending,
  int? assignedTo,
}) =>
    OrderView(
      id: id,
      merchantName: '商家$id',
      merchantLat: 30.5077,
      merchantLng: 114.4154,
      customerName: '顾客$id',
      customerAddress: '光谷一路 $id 号',
      remark: '',
      customerLat: 30.5120,
      customerLng: 114.4210,
      distanceM: 2400,
      promisedMinutes: 30,
      tier: OrderTier.normal,
      createdAtMin: 600,
      claimDeadlineMin: 605,
      pickedUpAtMin: null,
      deliveredAtMin: null,
      fee: const FeeView(
        baseFeeCents: 500,
        distanceExtraCents: 200,
        rainBonusCents: 0,
        levelBonusCents: 0,
        surgeMultiplier: 1,
        commissionCents: 100,
        quotedFeeCents: 600,
        takehomeCents: 500,
      ),
      redispatchCount: 0,
      surchargeCents: 0,
      status: status,
      assignedTo: assignedTo,
      forced: false,
      assignMode: null,
      backOrder: false,
      urgeCount: 0,
      prepMinutes: null,
      task: null,
    );

/// 测试用的时间尺度：生产是 8s / 8s / 1.5s / 1.2s，这里压到几十毫秒。
GameState _state({List<OrderView>? hall, void Function()? onResync}) {
  final s = GameState(
    claimAckTimeout: const Duration(milliseconds: 40),
    claimConfirmTimeout: const Duration(milliseconds: 40),
    claimSuccessHold: const Duration(milliseconds: 60),
    claimGoneHold: const Duration(milliseconds: 30),
  );
  s
    ..riderId = _riderId
    ..status = ConnStatus.online
    ..onResyncRequested = onResync;
  if (hall != null) s.hall = hall;
  return s;
}

void _ackOk(GameState s, int orderId, {int? riderId}) {
  s.applyFrame(ServerFrame(Ev.ack, {
    'action': Msg.claim,
    'ok': true,
    'data': {'order_id': orderId},
  }));
  s.applyFrame(ServerFrame(Ev.orderClaimed, {
    'order_id': orderId,
    'rider_id': riderId ?? _riderId,
    'rider_name': '你',
    'quoted_fee_cents': 600,
  }));
}

void _ackErr(GameState s, int orderId, String code, {String? message}) {
  s.applyFrame(ServerFrame(Ev.ack, {
    'action': Msg.claim,
    'ok': false,
    'code': code,
    'message': message ?? ErrCode.humanize(code),
    'data': {'order_id': orderId},
  }));
}

Map<String, dynamic> _orderJson(
  int id, {
  String status = 'pending',
  int? assignedTo,
}) =>
    {
      'id': id,
      'merchant_name': '商家$id',
      'merchant_lat': 30.5077,
      'merchant_lng': 114.4154,
      'customer_name': '顾客$id',
      'customer_address': '光谷一路 $id 号',
      'customer_lat': 30.5120,
      'customer_lng': 114.4210,
      'distance_m': 2400,
      'promised_minutes': 30,
      'tier': 'normal',
      'created_at_min': 600,
      'claim_deadline_min': 605,
      'fee': {
        'base_fee_cents': 500,
        'distance_extra_cents': 200,
        'rain_bonus_cents': 0,
        'level_bonus_cents': 0,
        'surge_multiplier': 1,
        'commission_cents': 100,
        'quoted_fee_cents': 600,
        'takehome_cents': 500,
      },
      'status': status,
      'assigned_to': assignedTo,
    };

void _snapshot(
  GameState s, {
  List<Map<String, dynamic>> hall = const [],
  List<Map<String, dynamic>> active = const [],
}) {
  s.applyFrame(ServerFrame(Ev.snapshot, {
    'hall': hall,
    'active_orders': active,
  }));
}

Widget _harness(GameState state, GameClient client, OrderView order) {
  return MaterialApp(
    home: Scaffold(
      body: AnimatedBuilder(
        animation: state,
        builder: (_, __) => ListView(
          children: [
            OrderCard(
              order: order,
              state: state,
              client: client,
              mode: OrderCardMode.hall,
            ),
          ],
        ),
      ),
    ),
  );
}

void main() {
  group('抢单状态机', () {
    test('提交进入 submitting；同一单不能重复提交', () {
      final s = _state(hall: [_order(1)]);
      expect(s.beginClaim(s.hall.first), isTrue);
      final a = s.claimAttemptFor(1)!;
      expect(a.phase, ClaimPhase.submitting);
      expect(a.gen, 1);
      expect(s.beginClaim(s.hall.first), isFalse);
      expect(s.claimAttemptFor(1)!.gen, 1);
      s.dispose();
    });

    test('离线时不发请求，提示网络错误且允许重试', () {
      final s = _state(hall: [_order(1)])..status = ConnStatus.retrying;
      expect(s.beginClaim(s.hall.first), isFalse);
      final a = s.claimAttemptFor(1)!;
      expect(a.phase, ClaimPhase.failed);
      expect(a.message, contains('网络'));
      expect(a.retryable, isTrue);
      s.dispose();
    });

    test('失败后可以再滑一次，gen 递增用来重置滑块', () {
      final s = _state(hall: [_order(1)]);
      s.beginClaim(s.hall.first);
      _ackErr(s, 1, ErrCode.heldLimitReached);
      expect(s.claimAttemptFor(1)!.phase, ClaimPhase.failed);
      expect(s.beginClaim(s.hall.first), isTrue);
      expect(s.claimAttemptFor(1)!.gen, 2);
      s.dispose();
    });

    test('ack 成功 + 事件：待取立即出现，大厅卡片留到成功态结束', () async {
      final alerts = <RiderAlert>[];
      final s = _state(hall: [_order(1)]);
      s.onAlert = alerts.add;
      s.beginClaim(s.hall.first);
      _ackOk(s, 1);

      expect(s.claimAttemptFor(1)!.phase, ClaimPhase.succeeded);
      expect(s.activeOrders.any((o) => o.id == 1), isTrue);
      expect(s.hall.any((o) => o.id == 1), isTrue,
          reason: '成功态要在原位置停留');
      expect(alerts.where((a) => a.kind == AlertKind.claimSuccess), hasLength(1));

      await Future<void>.delayed(const Duration(milliseconds: 120));
      expect(s.hall.any((o) => o.id == 1), isFalse);
      expect(s.activeOrders.any((o) => o.id == 1), isTrue,
          reason: '收走的只是大厅那张复制卡');
      s.dispose();
    });

    test('同一单的成功音效与文案只出现一次（重复 ack / 事件重放）', () {
      final alerts = <RiderAlert>[];
      final s = _state(hall: [_order(1)]);
      s.onAlert = alerts.add;
      s.beginClaim(s.hall.first);
      _ackOk(s, 1);
      _ackOk(s, 1);
      s.applyFrame(const ServerFrame(Ev.orderClaimed, {
        'order_id': 1,
        'rider_id': _riderId,
        'rider_name': '你',
      }));
      expect(alerts.where((a) => a.kind == AlertKind.claimSuccess), hasLength(1));
      expect(
        s.logs.where((l) => l.text.contains('抢单成功')).length,
        1,
        reason: '事件那行"抢到了"不算成功文案',
      );
      s.dispose();
    });

    test('被抢走：原因明确、不恢复滑块、短暂停留后移除', () async {
      final s = _state(hall: [_order(1), _order(2)]);
      s.beginClaim(s.hall.first);
      _ackErr(s, 1, ErrCode.alreadyClaimed, message: '单被其他骑手抢走了');

      final a = s.claimAttemptFor(1)!;
      expect(a.phase, ClaimPhase.failed);
      expect(a.message, contains('抢走'));
      expect(a.retryable, isFalse);
      expect(s.claimAttemptFor(2), isNull, reason: '两单互不干扰');

      await Future<void>.delayed(const Duration(milliseconds: 60));
      expect(s.hall.any((o) => o.id == 1), isFalse);
      expect(s.hall.any((o) => o.id == 2), isTrue);
      s.dispose();
    });

    test('容量不足 / 每日上限 / 超出范围：三种原因分开且都可重试', () {
      final s = _state(hall: [_order(1)]);
      s.beginClaim(s.hall.first);
      _ackErr(s, 1, ErrCode.heldLimitReached);
      expect(s.claimAttemptFor(1)!.message, contains('手上'));
      expect(s.claimAttemptFor(1)!.retryable, isTrue);

      s.beginClaim(s.hall.first);
      _ackErr(s, 1, ErrCode.dailyLimitReached);
      expect(s.claimAttemptFor(1)!.message, contains('今日'));
      expect(s.claimAttemptFor(1)!.retryable, isTrue);

      var resync = 0;
      s.onResyncRequested = () => resync++;
      s.beginClaim(s.hall.first);
      _ackErr(s, 1, ErrCode.outOfRange);
      expect(s.claimAttemptFor(1)!.message, contains('范围'));
      expect(s.claimAttemptFor(1)!.retryable, isTrue);
      expect(resync, greaterThan(0), reason: '范围变了要尽快对齐大厅');
      s.dispose();
    });

    test('ack 超时转确认中：请求快照、文案区分、禁止重复提交', () async {
      var resync = 0;
      final s = _state(hall: [_order(1)], onResync: () => resync++);
      s.beginClaim(s.hall.first);
      await Future<void>.delayed(const Duration(milliseconds: 70));

      expect(s.claimAttemptFor(1)!.phase, ClaimPhase.confirming);
      expect(resync, 1);
      expect(s.beginClaim(s.hall.first), isFalse,
          reason: '结果未知时不允许再提交');
      s.dispose();
    });

    test('超时但事件证明已分给我：最终识别为成功', () async {
      final alerts = <RiderAlert>[];
      final s = _state(hall: [_order(1)]);
      s.onAlert = alerts.add;
      s.beginClaim(s.hall.first);
      await Future<void>.delayed(const Duration(milliseconds: 70));
      expect(s.claimAttemptFor(1)!.phase, ClaimPhase.confirming);

      s.applyFrame(const ServerFrame(Ev.orderClaimed, {
        'order_id': 1,
        'rider_id': _riderId,
        'rider_name': '你',
      }));
      expect(s.claimAttemptFor(1)!.phase, ClaimPhase.succeeded);
      expect(alerts.where((a) => a.kind == AlertKind.claimSuccess), hasLength(1));
      s.dispose();
    });

    test('快照显示单已在我名下：确认中→成功', () async {
      final s = _state(hall: [_order(1)]);
      s.beginClaim(s.hall.first);
      await Future<void>.delayed(const Duration(milliseconds: 70));

      _snapshot(s, active: [
        _orderJson(1, status: 'assigned', assignedTo: _riderId),
      ]);
      expect(s.claimAttemptFor(1)!.phase, ClaimPhase.succeeded);
      s.dispose();
    });

    test('快照显示单还挂在墙上：确认中→可重试失败', () async {
      final s = _state(hall: [_order(1)]);
      s.beginClaim(s.hall.first);
      await Future<void>.delayed(const Duration(milliseconds: 70));

      _snapshot(s, hall: [_orderJson(1)]);
      final a = s.claimAttemptFor(1)!;
      expect(a.phase, ClaimPhase.failed);
      expect(a.retryable, isTrue);
      expect(a.message, contains('没有成功'));
      s.dispose();
    });

    test('两单并发：一成一败互不影响', () async {
      final s = _state(hall: [_order(1), _order(2)]);
      s.beginClaim(s.hall[0]);
      s.beginClaim(s.hall[1]);
      expect(s.claimAttemptFor(1)!.phase, ClaimPhase.submitting);
      expect(s.claimAttemptFor(2)!.phase, ClaimPhase.submitting);

      _ackOk(s, 1);
      expect(s.claimAttemptFor(1)!.phase, ClaimPhase.succeeded);
      expect(s.claimAttemptFor(2)!.phase, ClaimPhase.submitting,
          reason: '另一单还在等自己的回包');

      _ackErr(s, 2, ErrCode.orderExpired);
      expect(s.claimAttemptFor(2)!.phase, ClaimPhase.failed);
      expect(s.claimAttemptFor(1)!.phase, ClaimPhase.succeeded);
      s.dispose();
    });
  });

  group('订单卡抢单区', () {
    testWidgets('初始是原滑块；滑完立即换成居中 Loading', (tester) async {
      final state = _state(hall: [_order(1)]);
      final client = GameClient(state);
      final order = state.hall.first;
      addTearDown(state.dispose);
      tester.view.physicalSize = const Size(1080, 2400);
      tester.view.devicePixelRatio = 3;
      addTearDown(tester.view.reset);

      await tester.pumpWidget(_harness(state, client, order));
      expect(find.byType(SlideToConfirm), findsOneWidget);
      expect(find.text('滑动抢单'), findsOneWidget);
      expect(
        tester.getSize(find.byKey(const ValueKey('claim-1-0'))).height,
        44,
      );

      await tester.drag(find.byIcon(Icons.pan_tool_alt), const Offset(400, 0));
      await tester.pump();

      expect(find.byType(SlideToConfirm), findsNothing);
      expect(find.byType(CircularProgressIndicator), findsOneWidget);
      expect(find.text('抢单中…'), findsOneWidget);
      expect(
        tester.getSize(find.byKey(const ValueKey('claim-1-1'))).height,
        44,
        reason: '等待态占用的高度与滑块一致，卡片不跳',
      );
      expect(state.claimAttemptFor(1)!.phase, ClaimPhase.submitting);

      // 等待期间没有滑块可滑，自然不会重复提交。
      expect(find.byIcon(Icons.pan_tool_alt), findsNothing);

      // 把测试用的 40ms 计时器跑完，避免「树已销毁但定时器还在」的断言。
      await tester.pump(const Duration(milliseconds: 300));
    });

    testWidgets('成功后显示勾选与两行文案，待取列表同时更新', (tester) async {
      final state = _state(hall: [_order(1)]);
      final client = GameClient(state);
      final order = state.hall.first;
      addTearDown(state.dispose);
      tester.view.physicalSize = const Size(1080, 2400);
      tester.view.devicePixelRatio = 3;
      addTearDown(tester.view.reset);

      await tester.pumpWidget(_harness(state, client, order));
      await tester.drag(find.byIcon(Icons.pan_tool_alt), const Offset(400, 0));
      await tester.pump();
      _ackOk(state, 1);
      await tester.pump();

      expect(find.text('抢单成功！'), findsOneWidget);
      expect(find.text('请按照要求进行配送'), findsOneWidget);
      expect(find.byIcon(Icons.check_circle), findsOneWidget);
      expect(find.byType(CircularProgressIndicator), findsNothing);
      expect(state.activeOrders.any((o) => o.id == 1), isTrue);

      // 1.5s（测试里 60ms）后收走大厅那张卡，待取列表保留。
      await tester.pump(const Duration(milliseconds: 200));
      expect(state.hall.any((o) => o.id == 1), isFalse);
      expect(state.activeOrders.any((o) => o.id == 1), isTrue);
    });

    testWidgets('可重试失败回到滑块，原因写在滑块文案里', (tester) async {
      final state = _state(hall: [_order(1)]);
      final client = GameClient(state);
      final order = state.hall.first;
      addTearDown(state.dispose);
      tester.view.physicalSize = const Size(1080, 2400);
      tester.view.devicePixelRatio = 3;
      addTearDown(tester.view.reset);

      await tester.pumpWidget(_harness(state, client, order));
      await tester.drag(find.byIcon(Icons.pan_tool_alt), const Offset(400, 0));
      await tester.pump();
      _ackErr(state, 1, ErrCode.heldLimitReached);
      await tester.pump();

      expect(find.byType(SlideToConfirm), findsOneWidget);
      expect(find.textContaining('手上的单太多了'), findsOneWidget);
      expect(
        tester.getSize(find.byKey(const ValueKey('claim-1-1'))).height,
        44,
      );
    });

    testWidgets('被抢走：不再给滑块，过期后卡片移除', (tester) async {
      final state = _state(hall: [_order(1)]);
      final client = GameClient(state);
      final order = state.hall.first;
      addTearDown(state.dispose);
      tester.view.physicalSize = const Size(1080, 2400);
      tester.view.devicePixelRatio = 3;
      addTearDown(tester.view.reset);

      await tester.pumpWidget(_harness(state, client, order));
      await tester.drag(find.byIcon(Icons.pan_tool_alt), const Offset(400, 0));
      await tester.pump();
      _ackErr(state, 1, ErrCode.alreadyClaimed, message: '手慢了，单被抢走了');
      await tester.pump();

      expect(find.byType(SlideToConfirm), findsNothing);
      expect(find.textContaining('抢走'), findsOneWidget);

      await tester.pump(const Duration(milliseconds: 80));
      expect(state.hall.any((o) => o.id == 1), isFalse);
    });

    testWidgets('页面销毁后定时器不再回调', (tester) async {
      final state = _state(hall: [_order(1)]);
      final client = GameClient(state);
      final order = state.hall.first;
      tester.view.physicalSize = const Size(1080, 2400);
      tester.view.devicePixelRatio = 3;
      addTearDown(tester.view.reset);

      await tester.pumpWidget(_harness(state, client, order));
      await tester.drag(find.byIcon(Icons.pan_tool_alt), const Offset(400, 0));
      await tester.pump();
      expect(state.claimAttemptFor(1)!.phase, ClaimPhase.submitting);

      await tester.pumpWidget(const SizedBox.shrink());
      state.dispose();
      await tester.pump(const Duration(seconds: 1));
      expect(tester.takeException(), isNull);
    });
  });
}

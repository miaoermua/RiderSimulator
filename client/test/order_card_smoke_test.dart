import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rider_client/game_client.dart';
import 'package:rider_client/game_state.dart';
import 'package:rider_client/protocol.dart';
import 'package:rider_client/widgets/order_card.dart';
import 'package:rider_client/widgets/slide_confirm.dart';

OrderView _order({OrderStatus status = OrderStatus.assigned}) => OrderView(
      id: 1,
      merchantName: '老王牛肉面',
      merchantLat: 30.5077,
      merchantLng: 114.4154,
      customerName: '张三',
      customerAddress: '光谷一路 1 号',
      remark: '不要香菜',
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
      assignedTo: 1,
      forced: false,
      assignMode: null,
      backOrder: false,
      urgeCount: 0,
      prepMinutes: null,
      task: null,
    );

void main() {
  testWidgets('订单卡渲染，左侧步进条能展开地图且不崩', (tester) async {
    final state = GameState();
    final client = GameClient(state);
    tester.view.physicalSize = const Size(1080, 2400);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ListView(
            children: [
              OrderCard(
                order: _order(),
                state: state,
                client: client,
                mode: OrderCardMode.pickup,
              ),
            ],
          ),
        ),
      ),
    );

    expect(find.text('老王牛肉面'), findsOneWidget);
    expect(find.text('取'), findsOneWidget);
    expect(tester.takeException(), isNull);

    // 点左侧中间那块：展开地图。
    await tester.tap(find.byIcon(Icons.map_outlined));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.byIcon(Icons.map), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('大厅与待送卡片在 360 宽下不溢出', (tester) async {
    final state = GameState()..riderId = 1;
    final client = GameClient(state);
    tester.view.physicalSize = const Size(1080, 2400);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ListView(
            children: [
              OrderCard(
                order: _order(status: OrderStatus.pending),
                state: state,
                client: client,
                mode: OrderCardMode.hall,
              ),
              OrderCard(
                order: _order(status: OrderStatus.pickedUp),
                state: state,
                client: client,
                mode: OrderCardMode.delivery,
              ),
            ],
          ),
        ),
      ),
    );

    expect(find.text('滑动抢单'), findsOneWidget);
    expect(find.text('滑动确认送达'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('滑动确认：不到右端不触发，拖到底才触发', (tester) async {
    var hits = 0;
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: Center(
            child: SizedBox(
              width: 220,
              height: 44,
              child: SlideToConfirm(
                label: '滑动抢单',
                onConfirm: () => hits++,
              ),
            ),
          ),
        ),
      ),
    );

    // 手滑一点点就松手：不该抢。
    await tester.drag(find.byIcon(Icons.arrow_forward), const Offset(50, 0));
    await tester.pumpAndSettle();
    expect(hits, 0);

    // 拖到最右：触发一次。
    await tester.drag(find.byIcon(Icons.arrow_forward), const Offset(400, 0));
    await tester.pumpAndSettle();
    expect(hits, 1);
    expect(find.byIcon(Icons.check), findsOneWidget);

    // 已经确认过，再拖也不重复触发。
    await tester.drag(find.byIcon(Icons.check), const Offset(-200, 0));
    await tester.pumpAndSettle();
    expect(hits, 1);
  });
}

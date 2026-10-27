import 'package:flutter/material.dart';
import 'package:flutter_map/flutter_map.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:latlong2/latlong.dart';
import 'package:rider_client/game_state.dart';
import 'package:rider_client/protocol.dart';
import 'package:rider_client/widgets/delivery_map.dart';

OrderView _order() => const OrderView(
      id: 1,
      merchantName: '老王牛肉面',
      merchantLat: 30.5077,
      merchantLng: 114.4154,
      customerName: '张三',
      customerAddress: '光谷一路 1 号',
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
      fee: FeeView(
        baseFeeCents: 500,
        distanceExtraCents: 0,
        rainBonusCents: 0,
        levelBonusCents: 0,
        surgeMultiplier: 1,
        commissionCents: 100,
        quotedFeeCents: 600,
        takehomeCents: 500,
      ),
      redispatchCount: 0,
      surchargeCents: 0,
      status: OrderStatus.assigned,
      assignedTo: 1,
      forced: false,
      assignMode: null,
      backOrder: false,
      urgeCount: 0,
      prepMinutes: null,
      task: null,
    );

Future<void> _pumpMap(WidgetTester tester, {required LatLng focus}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: Center(
          child: SizedBox(
            width: 360,
            height: 170,
            child: DeliveryMap(
              state: GameState(),
              orders: [_order()],
              showMerchants: false,
              showBasemapPicker: false,
              focus: focus,
            ),
          ),
        ),
      ),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 100));
}

void main() {
  // 覆盖不到的：瓦片失败后的重载路径。`flutter test` 的 mock HTTP 既不渲染出
  // `Image`，也不把错误回调送到 `TileLayer.errorTileCallback`（实测 `images=0`
  // 且 `takeException()` 为 null，在 `runAsync` 里等 3 秒仍没有任何回调），
  // 所以这条只能靠真机断网验，不在这里放一个空测试假装绿。
  testWidgets('首帧相机就落在 focus 点', (tester) async {
    // 取货点（fitPoints.first）故意和 focus 分开：
    // 改之前首帧落在取货点，改之后必须直接落在 focus。
    const customer = LatLng(30.5120, 114.4210);
    await _pumpMap(tester, focus: customer);

    final map = tester.widget<FlutterMap>(find.byType(FlutterMap));
    // 首帧的相机种子。只断言「最后停在 focus」是假绿：
    // 那一帧之后本来就有 `move(focus, 16)`，改前改后都过。
    expect(
      map.options.initialCenter,
      customer,
      reason: '初始中心就该是目标点，不该先按取货点算一遍瓦片',
    );
    expect(map.options.initialCenter, isNot(const LatLng(30.5077, 114.4154)));

    expect(map.mapController, isNotNull);
    expect(map.mapController!.camera.center, customer, reason: '镜头最终看着目标点');

    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump(const Duration(seconds: 10));
  });
}

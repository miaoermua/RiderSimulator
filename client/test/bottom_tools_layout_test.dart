import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rider_client/game_client.dart';
import 'package:rider_client/game_state.dart';
import 'package:rider_client/screens/rider_home.dart';
import 'package:shared_preferences/shared_preferences.dart';

/// 底部工具栏的布局测试。
///
/// 只 pump `BottomTools` 一个组件，不 pump 整个首页：首页挂着定时器、
/// 定位流和连接状态，为了量一次按钮宽度去启动那一整坨既慢又不稳。
void main() {
  setUp(() {
    SharedPreferences.setMockInitialValues({});
  });

  /// 把底栏挂到一个能跟随 state 重建的外壳里——真实场景下这是首页的
  /// `AnimatedBuilder`。少了这层，改 state 不会触发重绘，量出来的还是旧值。
  Future<void> pumpTools(
    WidgetTester tester,
    GameState state, {
    Size size = const Size(390, 844),
  }) async {
    tester.view.physicalSize = size;
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    await tester.pumpWidget(
      MaterialApp(
        home: AnimatedBuilder(
          animation: state,
          builder: (context, _) => Scaffold(
            body: const SizedBox.expand(),
            bottomNavigationBar: BottomTools(
              state: state,
              client: GameClient(state),
              onRefresh: () async {},
              onRoutePlan: () {},
            ),
          ),
        ),
      ),
    );
    await tester.pump();
  }

  /// 刷新按钮（底栏里唯一的 FilledButton）。
  Rect refreshRect(WidgetTester tester) =>
      tester.getRect(find.byType(FilledButton));

  testWidgets('平台偏好关闭：底栏保留路线规划入口', (tester) async {
    final state = GameState();
    await pumpTools(tester, state);
    expect(find.text('路线规划'), findsOneWidget);
  });

  testWidgets('平台偏好开启：路线规划移出底栏（改挂到 TabBar 右侧）', (tester) async {
    final state = GameState();
    await state.setPlatformPref(true);
    await pumpTools(tester, state);
    expect(find.text('路线规划'), findsNothing);
  });

  testWidgets('平台偏好开启：刷新右边缘向左收，留出右侧空间', (tester) async {
    // 注意这里**不能**拿"开启后"和"关闭时"比宽度：
    // 开启后少了一个按钮，剩余空间更多，刷新按比例反而可能更宽。
    // 真正要验的是**右边缘的位置**——它必须离屏幕右边更远。
    const screenW = 390.0;
    const pagePadding = 8.0;
    const reserved = 20.0;

    final state = GameState();
    await pumpTools(tester, state);
    final gapOff = screenW - refreshRect(tester).right;

    await state.setPlatformPref(true);
    await tester.pump();
    final gapOn = screenW - refreshRect(tester).right;

    expect(
      gapOff,
      lessThan(pagePadding + 2),
      reason: '关闭时不受影响，右边缘只差一个页面内边距',
    );
    expect(
      gapOn,
      greaterThanOrEqualTo(pagePadding + reserved - 2),
      reason: '开启后右侧要留出 20px 空白',
    );
    expect(gapOn, greaterThan(gapOff), reason: '这就是"向左移动"');
  });

  testWidgets('两种模式下刷新按钮都保持合理的触控高度', (tester) async {
    final state = GameState();
    await pumpTools(tester, state);
    expect(refreshRect(tester).height, greaterThanOrEqualTo(44));

    await state.setPlatformPref(true);
    await tester.pump();
    expect(
      refreshRect(tester).height,
      greaterThanOrEqualTo(44),
      reason: '压缩的是宽度分配，不是高度——按得到才算数',
    );
  });

  testWidgets('小屏（320 宽）两种模式都不溢出', (tester) async {
    final state = GameState();
    await pumpTools(tester, state, size: const Size(320, 568));
    expect(tester.takeException(), isNull);

    await state.setPlatformPref(true);
    await tester.pump();
    expect(tester.takeException(), isNull, reason: '开启后更窄，更不能溢出');
  });

  testWidgets('平台偏好关闭时布局与调整前一致（刷新占六成）', (tester) async {
    // 关掉时不该受新逻辑影响：右边缘应当一直顶到屏幕右边（只有 8px 内边距）。
    final state = GameState();
    await pumpTools(tester, state);
    final rect = refreshRect(tester);
    expect(390.0 - rect.right, lessThan(10), reason: '只有 8px 页面内边距');
  });
}

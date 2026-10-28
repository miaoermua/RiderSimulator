import 'package:flutter_test/flutter_test.dart';
import 'package:rider_client/protocol.dart';

/// 快照里骑手的最小合法 JSON。只写 `RiderView.fromJson` 要求的字段，
/// 财务字段留空（公开版本来就不带）。
Map<String, dynamic> _rider({required List<int> held, int? heldCap}) => {
      'id': 7,
      'key': 'r7',
      'name': '张三',
      'kind': 'human',
      'online': true,
      'injured': false,
      'frozen': false,
      'speed_mode': 'normal',
      'level': 1,
      'rating': 5.0,
      'completed': 3,
      'rejects_today': 0,
      'forced_offline': false,
      'held': held,
      if (heldCap != null) 'held_cap': heldCap,
      'phase': null,
      'lat': 30.0,
      'lng': 120.0,
    };

void main() {
  group('背单容量展示', () {
    test('解析服务端下发的生效上限', () {
      final me = RiderView.fromJson(_rider(held: [1, 2], heldCap: 3));
      expect(me.heldCap, 3);
      expect(heldCapacityLabel(me, 5), '2/3');
      expect(riderAtCapacity(me, 5), isFalse);
    });

    test('手上单量达到上限即满载', () {
      final me = RiderView.fromJson(_rider(held: [1, 2, 3], heldCap: 3));
      expect(riderAtCapacity(me, 5), isTrue);
    });

    test('上限为 0 也算满载：只允许送完手上的', () {
      final empty = RiderView.fromJson(_rider(held: [], heldCap: 0));
      expect(riderAtCapacity(empty, 5), isTrue);
      expect(heldCapacityLabel(empty, 5), '0/0');

      final carrying = RiderView.fromJson(_rider(held: [1], heldCap: 0));
      expect(riderAtCapacity(carrying, 5), isTrue);
    });

    test('覆盖值优先于配置值', () {
      final me = RiderView.fromJson(_rider(held: [1, 2], heldCap: 2));
      // 配置说还能背 3 单，但服务端生效上限是 2 —— 以快照为准。
      expect(riderAtCapacity(me, 3), isTrue);
    });

    test('老服务端没下发 held_cap 时退回配置值', () {
      final me = RiderView.fromJson(_rider(held: [1, 2]));
      expect(me.heldCap, isNull);
      expect(effectiveHeldCap(me, 3), 3);
      expect(riderAtCapacity(me, 3), isFalse);
      expect(riderAtCapacity(me, 2), isTrue);
      expect(heldCapacityLabel(me, 3), '2/3');
    });

    test('还不知道自己是谁时不判定满载', () {
      expect(effectiveHeldCap(null, 3), isNull);
      expect(riderAtCapacity(null, 3), isFalse);
      expect(heldCapacityLabel(null, 3), '--');
    });
  });
}

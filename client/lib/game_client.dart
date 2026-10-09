import 'dart:async';
import 'dart:convert';

import 'package:web_socket_channel/web_socket_channel.dart';

import 'game_state.dart';
import 'protocol.dart';

/// WebSocket 连接管理：握手、收帧、断线重连、上行指令。
///
/// 它**不保存游戏状态**——解析出来的帧全部丢给 [GameState]，
/// 这样重连后状态能靠服务端快照自愈。
class GameClient {
  GameClient(this.state);

  final GameState state;

  WebSocketChannel? _channel;
  StreamSubscription<dynamic>? _sub;
  Timer? _retryTimer;
  Timer? _pingTimer;
  int _attempt = 0;
  bool _disposed = false;

  String _url = kDefaultServerUrl;
  String? _token;

  /// 连接并在成功后自动发送 `hello`。失败会自动重试。
  void connect(String url, {String? token}) {
    _url = url;
    _token = (token == null || token.trim().isEmpty) ? null : token.trim();
    _attempt = 0;
    _open();
  }

  /// 主动重连（用户点「重连」时用）。
  void reconnect() {
    _closeSocket();
    _open();
  }

  Future<void> _open() async {
    if (_disposed) return;
    state.setStatus(_attempt == 0 ? ConnStatus.connecting : ConnStatus.retrying);
    try {
      final channel = WebSocketChannel.connect(Uri.parse(_url));
      await channel.ready;
      if (_disposed) {
        await channel.sink.close();
        return;
      }
      _channel = channel;
      _attempt = 0;
      state.setStatus(ConnStatus.online);
      state.note('已连接 $_url', kind: LogKind.good);

      _sub = channel.stream.listen(
        _onRaw,
        onError: (Object e) => _fail('连接出错：$e'),
        onDone: () => _fail('连接已断开'),
        cancelOnError: true,
      );

      _send({ 'type': Msg.hello, 'token': _token, 'protocol_version': kProtocolVersion });

      _pingTimer?.cancel();
      _pingTimer = Timer.periodic(const Duration(seconds: 20), (_) {
        _send({ 'type': Msg.ping });
      });
    } catch (e) {
      _fail('连不上 $_url：$e');
    }
  }

  void _onRaw(dynamic raw) {
    if (raw is! String) return;
    Object? decoded;
    try {
      decoded = jsonDecode(raw);
    } catch (_) {
      state.note('收到无法解析的帧', kind: LogKind.bad);
      return;
    }
    final frame = parseFrame(decoded);
    if (frame == null) return;
    state.applyFrame(frame);
  }

  void _fail(String why) {
    if (_disposed) return;
    _closeSocket();
    state.setStatus(ConnStatus.retrying);
    state.note('$why，即将重连…', kind: LogKind.bad);

    _attempt++;
    final delay = Duration(seconds: (_attempt * 2).clamp(2, 20));
    _retryTimer?.cancel();
    _retryTimer = Timer(delay, _open);
  }

  void _closeSocket() {
    _pingTimer?.cancel();
    _pingTimer = null;
    _sub?.cancel();
    _sub = null;
    _channel?.sink.close();
    _channel = null;
  }

  // ---------------------------------------------------------------------
  // 上行
  // ---------------------------------------------------------------------

  void _send(Map<String, dynamic> msg) {
    final ch = _channel;
    if (ch == null) {
      state.note('还没连上，指令被丢弃', kind: LogKind.bad);
      return;
    }
    ch.sink.add(jsonEncode(msg));
  }

  void claim(int orderId, {bool backOrder = false}) =>
      _send({ 'type': Msg.claim, 'order_id': orderId, 'back_order': backOrder });

  void reject(int orderId) => _send({ 'type': Msg.reject, 'order_id': orderId });
  void urge(int orderId) => _send({ 'type': Msg.urge, 'order_id': orderId });
  void arriveShop(int orderId) =>
      _send({ 'type': Msg.arriveShop, 'order_id': orderId });
  void arriveCustomer(int orderId) =>
      _send({ 'type': Msg.arriveCustomer, 'order_id': orderId });
  void deliver(int orderId) =>
      _send({ 'type': Msg.deliver, 'order_id': orderId });

  void setOnline(bool online) =>
      _send({ 'type': Msg.setOnline, 'online': online });
  void setSpeedMode(SpeedMode mode) =>
      _send({ 'type': Msg.setSpeedMode, 'mode': mode.wire });
  void sendTaskMessage(int orderId, String text) =>
      _send({ 'type': Msg.taskMessage, 'order_id': orderId, 'text': text });

  void resync() => _send({ 'type': Msg.resync });

  void dispose() {
    _disposed = true;
    _retryTimer?.cancel();
    _closeSocket();
    state.setStatus(ConnStatus.closed);
  }
}

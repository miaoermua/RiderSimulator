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
  String? _riderName;
  String? _password;

  /// 名字 / 密码被服务端拒了。重试多少次都一样，得停下来让用户改。
  bool _rejected = false;
  String? _rejectReason;

  /// 握手成功了（收到 `welcome`）。登录页据此才肯放进主界面。
  bool _welcomed = false;

  /// 账号被拒时回调一次。登录页据此把错误留在屏幕上，而不是默默重连。
  void Function(String reason)? onAuthError;

  /// 握手成功时回调一次。
  void Function()? onWelcome;

  /// 连接并在成功后自动发送 `hello`。失败会自动重试。
  ///
  /// 两种身份二选一：`riderName` + `password`（烽火台建的号，正常路径），
  /// 或 `token`（`rider:<id>` / `new:<昵称>`，**开发后门**，会绕过账号表）。
  void connect(String url, {String? token, String? riderName, String? password}) {
    _url = url;
    _token = (token == null || token.trim().isEmpty) ? null : token.trim();
    _riderName = (riderName == null || riderName.trim().isEmpty) ? null : riderName.trim();
    _password = _riderName == null ? null : password;
    _rejected = false;
    _rejectReason = null;
    _welcomed = false;
    _attempt = 0;
    _open();
  }

  /// 主动重连（用户点「重连」时用）。
  void reconnect() {
    // 用户改完名字 / 密码再来一次，就别再卡在「上次被拒」上。
    _rejected = false;
    _rejectReason = null;
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

      // 只带上真正有的字段：服务端用「有没有 rider_name」决定走账号登录
      // 还是走令牌后门，塞个 null 进去会让它以为你想用账号登录。
      _send({
        'type': Msg.hello,
        'protocol_version': kProtocolVersion,
        if (_token != null) 'token': _token,
        if (_riderName != null) 'rider_name': _riderName,
        if (_password != null) 'password': _password,
      });

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

    final wasWelcomed = _welcomed;
    _rememberReject(frame);
    state.applyFrame(frame);
    if (!wasWelcomed && _welcomed) onWelcome?.call();
  }

  /// 分辨「握手被拒」与「连上之后的运行期报错」。
  ///
  /// 规则：`welcome` 之前来的 `error` 一律当握手失败——那时能出的错
  /// （HELLO_REQUIRED / PROTOCOL_MISMATCH / UNAUTHORIZED / NO_ACCOUNT /
  /// BAD_CREDENTIALS…）重试都不会变好。`welcome` 之后的报错是跑单过程中的
  /// 正常反馈（手慢了、没上线…），不该把用户踢下线。
  void _rememberReject(ServerFrame frame) {
    if (frame.event == Ev.welcome) {
      _welcomed = true;
      return;
    }
    if (frame.event != Ev.error || _welcomed) return;
    _rejected = true;
    _rejectReason = ErrCode.humanize(reqStr(frame.payload['code']));
  }

  void _fail(String why) {
    if (_disposed) return;
    _closeSocket();

    if (_rejected) {
      // 名字 / 密码不对，重连一万次也是同样的结果，停下来让用户改。
      // 错误本身已经由 `applyFrame` 记进日志了，这里不再重复记一条。
      state.setStatus(ConnStatus.closed);
      onAuthError?.call(_rejectReason ?? why);
      return;
    }

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
  /// 收下系统塞来的指派单。指派后订单已在名下，这一步只是摘掉「待确认」标记；
  /// 但必须发：不发，之后退单会被当成「拒指派」再吃一次额度。
  void acceptAssign(int orderId) =>
      _send({ 'type': Msg.acceptAssign, 'order_id': orderId });
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

  /// 改自己的密码。旧密码必须对——这是把二维码里的明文密码作废的唯一办法。
  void changePassword(String oldPassword, String newPassword) => _send({
        'type': Msg.changePassword,
        'old_password': oldPassword,
        'new_password': newPassword,
      });
  void sendTaskMessage(int orderId, String text) =>
      _send({ 'type': Msg.taskMessage, 'order_id': orderId, 'text': text });

  /// 订单会话（末端交付沟通）里说一句话。
  ///
  /// 与 [sendTaskMessage] 不同：**不要求订单挂任务**，末端任何一单都能说。
  /// 顾客会不会回由服务端决定——说了话不等于一定有人搭理。
  void sendChatSay(int orderId, String text) =>
      _send({ 'type': Msg.chatSay, 'order_id': orderId, 'text': text });

  /// 订单会话里点一条预设话术。文本由服务端按枚举补全，这里只发枚举名。
  void sendChatPreset(int orderId, ChatPreset preset) => _send({
        'type': Msg.chatPreset,
        'order_id': orderId,
        'preset': preset.wire,
      });

  void resync() => _send({ 'type': Msg.resync });

  void dispose() {
    _disposed = true;
    _retryTimer?.cancel();
    _closeSocket();
    state.setStatus(ConnStatus.closed);
  }
}

import 'package:flutter/material.dart';

import 'game_client.dart';
import 'game_state.dart';
import 'location_service.dart';
import 'screens/login_page.dart';
import 'screens/rider_home.dart';

void main() => runApp(const RiderApp());

/// 应用根：先登录，进主界面后保持同一条连接。
class RiderApp extends StatefulWidget {
  const RiderApp({super.key});

  @override
  State<RiderApp> createState() => _RiderAppState();
}

class _RiderAppState extends State<RiderApp> {
  final GameState _state = GameState();
  GameClient? _client;

  @override
  void initState() {
    super.initState();
    // 定位频率是本地偏好，跟账号无关，先读出来再决定要不要开流。
    LocationService.instance.loadSaved();
  }

  /// 已经发起了连接（等着握手结果）。
  bool _entered = false;

  /// 握手成功。**只有到这一步才放进主界面**——名字 / 密码不对时服务端会拒绝，
  /// 那时得把人留在登录页上看到原因，而不是丢进一个没有数据的骑手首页。
  bool _welcomed = false;

  /// 上一次登录被拒的原因。
  String? _loginError;

  void _enter(LoginRequest req) {
    final client = _client ??= GameClient(_state);

    client.onWelcome = () {
      if (!mounted) return;
      setState(() => _welcomed = true);
    };
    client.onAuthError = (reason) {
      if (!mounted) return;
      // 退回登录页，并把服务端给的原因留在那儿。
      setState(() {
        _entered = false;
        _welcomed = false;
        _loginError = reason;
      });
    };

    client.connect(
      req.url,
      token: req.token,
      riderName: req.riderName,
      password: req.password,
    );
    setState(() {
      _entered = true;
      _welcomed = false;
      _loginError = null;
    });
  }

  @override
  void dispose() {
    _client?.dispose();
    _state.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: '骑手模拟器',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFF1976D2)),
        useMaterial3: true,
      ),
      home: (_entered && _welcomed)
          ? RiderHomePage(state: _state, client: _client!)
          : LoginPage(onEnter: _enter, error: _loginError),
    );
  }
}

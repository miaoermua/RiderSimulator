import 'package:flutter/material.dart';

import 'game_client.dart';
import 'game_state.dart';
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
  bool _entered = false;

  void _enter(String url, String? token) {
    _client ??= GameClient(_state);
    _client!.connect(url, token: token);
    setState(() => _entered = true);
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
      home: _entered
          ? RiderHomePage(state: _state, client: _client!)
          : LoginPage(onEnter: _enter),
    );
  }
}

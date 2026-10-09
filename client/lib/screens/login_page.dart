import 'package:flutter/material.dart';

import '../protocol.dart';

/// 登录页：填服务端地址与令牌。
///
/// 令牌语义见 `packages/protocol.json`：`rider:<id>` 绑号、`new:<昵称>` 建号、留空观战。
class LoginPage extends StatefulWidget {
  const LoginPage({super.key, required this.onEnter});

  final void Function(String url, String? token) onEnter;

  @override
  State<LoginPage> createState() => _LoginPageState();
}

class _LoginPageState extends State<LoginPage> {
  late final TextEditingController _url;
  late final TextEditingController _token;

  @override
  void initState() {
    super.initState();
    _url = TextEditingController(text: kDefaultServerUrl);
    _token = TextEditingController(text: 'rider:1');
  }

  @override
  void dispose() {
    _url.dispose();
    _token.dispose();
    super.dispose();
  }

  void _enter() {
    final url = _url.text.trim();
    if (url.isEmpty) return;
    widget.onEnter(url, _token.text.trim());
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                const Icon(Icons.delivery_dining, size: 56),
                const SizedBox(height: 12),
                Text(
                  '骑手模拟器',
                  textAlign: TextAlign.center,
                  style: Theme.of(context).textTheme.headlineSmall,
                ),
                const SizedBox(height: 4),
                const Text(
                  '协议 $kProtocolVersion',
                  textAlign: TextAlign.center,
                  style: TextStyle(color: Colors.grey),
                ),
                const SizedBox(height: 24),
                TextField(
                  controller: _url,
                  decoration: const InputDecoration(
                    labelText: '服务端地址',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 12),
                TextField(
                  controller: _token,
                  decoration: const InputDecoration(
                    labelText: '令牌',
                    helperText: 'rider:1 绑号 · new:张三 建号 · 留空观战',
                    border: OutlineInputBorder(),
                  ),
                  onSubmitted: (_) => _enter(),
                ),
                const SizedBox(height: 16),
                Wrap(
                  spacing: 8,
                  children: [
                    ActionChip(
                      label: const Text('rider:1'),
                      onPressed: () => _token.text = 'rider:1',
                    ),
                    ActionChip(
                      label: const Text('new:张三'),
                      onPressed: () => _token.text = 'new:张三',
                    ),
                    ActionChip(
                      label: const Text('观战'),
                      onPressed: () => _token.text = '',
                    ),
                  ],
                ),
                const SizedBox(height: 20),
                FilledButton.icon(
                  onPressed: _enter,
                  icon: const Icon(Icons.login),
                  label: const Text('进入'),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

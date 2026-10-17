import 'package:flutter/material.dart';

import '../login_link.dart';
import '../login_store.dart';
import '../protocol.dart';
import 'scan_login_page.dart';

/// 一次登录尝试。
///
/// 两种身份二选一：有 [riderName] 就走账号（烽火台建的号），有 [token] 就走
/// **开发后门**（`rider:<id>` / `new:<昵称>`）；两者都没有 = 观战。
class LoginRequest {
  const LoginRequest({
    required this.url,
    this.token,
    this.riderName,
    this.password,
  });

  /// 已整理好的 WebSocket 地址。
  final String url;
  final String? token;
  final String? riderName;
  final String? password;
}

/// 登录页。
///
/// 正常路径：**扫烽火台的二维码**，或者手填「服务器地址 + 骑手名字 + 密码」。
/// 令牌是留的开发后门（令牌语义见 `packages/protocol.json`），藏在「高级」里，
/// 免得骑手看到一堆自己用不上的东西。
class LoginPage extends StatefulWidget {
  const LoginPage({super.key, required this.onEnter, this.error});

  final void Function(LoginRequest req) onEnter;

  /// 上一次登录被服务端拒了的原因，要一直摆在骑手眼前。
  final String? error;

  @override
  State<LoginPage> createState() => _LoginPageState();
}

class _LoginPageState extends State<LoginPage> {
  late final TextEditingController _host;
  late final TextEditingController _port;
  late final TextEditingController _name;
  late final TextEditingController _password;
  late final TextEditingController _token;

  bool _obscure = true;
  /// 本地校验的错（比如地址填错）。服务端拒的那条走 `widget.error`。
  String? _localError;

  @override
  void initState() {
    super.initState();
    _host = TextEditingController(text: kDefaultServerHost);
    _port = TextEditingController(text: '$kDefaultPort');
    _name = TextEditingController();
    _password = TextEditingController();
    _token = TextEditingController();
    _restore();
  }

  /// 把上次的登录信息填回表单。**只填，不自动进**——启动永远停在登录页。
  Future<void> _restore() async {
    final saved = await loadSavedLogin();
    if (!mounted || saved == null) return;
    setState(() {
      _host.text = saved.host;
      _port.text = saved.port;
      _name.text = saved.name;
      _password.text = saved.password;
      _token.text = saved.token ?? '';
    });
  }

  /// 记住这一份输入，下次打开还在。密码错也照存：错的密码下一把改一个字就行。
  void _remember() {
    saveLogin(
      SavedLogin(
        host: _host.text.trim(),
        port: _port.text.trim(),
        name: _name.text.trim(),
        password: _password.text,
        token: _token.text.trim(),
      ),
    );
  }

  @override
  void dispose() {
    _host.dispose();
    _port.dispose();
    _name.dispose();
    _password.dispose();
    _token.dispose();
    super.dispose();
  }

  /// 把「地址 + 端口」两格合成一个 WebSocket 地址。
  ///
  /// 骑手经常直接把烽火台给的整串粘进地址格（`ws://…` 或带端口的
  /// `192.168.1.7:9000`），那时端口格就该让位。
  String _resolveUrl() {
    final host = _host.text.trim();
    final port = _port.text.trim();
    if (host.contains('://') || host.contains(':')) {
      return normalizeServerUrl(host);
    }
    return normalizeServerUrl(port.isEmpty ? host : '$host:$port');
  }

  void _enter() {
    final url = _resolveUrl();
    if (url.isEmpty) {
      setState(() => _localError = '服务器地址填不对，看看烽火台上写的是哪个');
      return;
    }

    final name = _name.text.trim();
    final token = _token.text.trim();
    final password = _password.text;

    if (name.isNotEmpty && password.isEmpty) {
      setState(() => _localError = '填了名字就得填密码（在烽火台建号时设的那个）');
      return;
    }

    setState(() => _localError = null);
    _remember();
    widget.onEnter(
      LoginRequest(
        url: url,
        token: token.isEmpty ? null : token,
        riderName: name.isEmpty ? null : name,
        password: name.isEmpty ? null : password,
      ),
    );
  }

  /// 扫二维码。扫到的整串原样解析——和手填走的是同一条路径。
  Future<void> _scan() async {
    final raw = await Navigator.of(context).push<String>(
      MaterialPageRoute(builder: (_) => const ScanLoginPage()),
    );
    if (!mounted || raw == null) return;

    final link = LoginLink.parse(raw);
    if (link == null) {
      setState(() => _localError = '这个码不是骑手接入码');
      return;
    }

    // 码里带了完整地址（反代 / wss）就用它，否则退回主机 + 端口。
    _host.text = link.url ?? link.host;
    _port.text = '${link.port}';
    _name.text = link.name;
    _password.text = link.password;
    _enter();
  }

  @override
  Widget build(BuildContext context) {
    final error = _localError ?? widget.error;

    return Scaffold(
      body: SafeArea(
        child: Center(
          child: SingleChildScrollView(
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
                    const SizedBox(height: 20),

                    if (error != null) ...[
                      _ErrorBanner(text: error),
                      const SizedBox(height: 16),
                    ],

                    // 扫码在最上面：这是正常路径，手填是它的退路。
                    FilledButton.tonalIcon(
                      onPressed: _scan,
                      icon: const Icon(Icons.qr_code_scanner),
                      label: const Text('扫二维码登录'),
                    ),
                    const SizedBox(height: 16),
                    const Row(
                      children: [
                        Expanded(child: Divider()),
                        Padding(
                          padding: EdgeInsets.symmetric(horizontal: 12),
                          child: Text('或者手填', style: TextStyle(color: Colors.grey)),
                        ),
                        Expanded(child: Divider()),
                      ],
                    ),
                    const SizedBox(height: 12),

                    Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Expanded(
                          flex: 3,
                          child: TextField(
                            controller: _host,
                            keyboardType: TextInputType.url,
                            decoration: const InputDecoration(
                              labelText: '服务器地址',
                              border: OutlineInputBorder(),
                            ),
                          ),
                        ),
                        const SizedBox(width: 8),
                        Expanded(
                          child: TextField(
                            controller: _port,
                            keyboardType: TextInputType.number,
                            decoration: const InputDecoration(
                              labelText: '端口',
                              border: OutlineInputBorder(),
                            ),
                          ),
                        ),
                      ],
                    ),
                    const SizedBox(height: 12),
                    TextField(
                      controller: _name,
                      decoration: const InputDecoration(
                        labelText: '骑手名字',
                        helperText: '烽火台上给你建号时用的名字',
                        border: OutlineInputBorder(),
                      ),
                    ),
                    const SizedBox(height: 12),
                    TextField(
                      controller: _password,
                      obscureText: _obscure,
                      decoration: InputDecoration(
                        labelText: '密码',
                        border: const OutlineInputBorder(),
                        suffixIcon: IconButton(
                          icon: Icon(_obscure ? Icons.visibility : Icons.visibility_off),
                          onPressed: () => setState(() => _obscure = !_obscure),
                        ),
                      ),
                      onSubmitted: (_) => _enter(),
                    ),

                    const SizedBox(height: 20),
                    FilledButton.icon(
                      onPressed: _enter,
                      icon: const Icon(Icons.login),
                      label: const Text('进入'),
                    ),

                    // 开发后门藏起来：骑手不该在这里纠结填什么。
                    Theme(
                      data: Theme.of(context).copyWith(dividerColor: Colors.transparent),
                      child: ExpansionTile(
                        tilePadding: EdgeInsets.zero,
                        title: const Text('高级（开发用）', style: TextStyle(fontSize: 14)),
                        children: [
                          TextField(
                            controller: _token,
                            decoration: const InputDecoration(
                              labelText: '令牌',
                              helperText: 'rider:1 绑号 · new:张三 建号 · 留空观战',
                              helperMaxLines: 2,
                              border: OutlineInputBorder(),
                            ),
                            onSubmitted: (_) => _enter(),
                          ),
                          const SizedBox(height: 8),
                          const Text(
                            '令牌会绕过烽火台的账号表，只在开发时用。',
                            style: TextStyle(fontSize: 12, color: Colors.grey),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 一条摆在表单顶上的错误。
class _ErrorBanner extends StatelessWidget {
  const _ErrorBanner({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: scheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: [
          Icon(Icons.error_outline, color: scheme.onErrorContainer, size: 20),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              text,
              style: TextStyle(color: scheme.onErrorContainer),
            ),
          ),
        ],
      ),
    );
  }
}

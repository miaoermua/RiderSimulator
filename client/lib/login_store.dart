/// 登录配置的本地留存。
///
/// 只做「下次打开登录页还认得你」，**不做自动登录**：应用启动永远停在登录页，
/// 由骑手自己点「进入」。这样换班 / 换号时不会莫名其妙被塞进上一个人的会话。
///
/// 存的是明文密码——这个 App 连的本来就是局域网里的模拟服务端，
/// 二维码里本身也有明文密码（见 `login_link.dart` 的取舍说明）。
/// 真机上这份配置躺在应用私有目录，不 root / 不越狱读不到。
library;

import 'package:shared_preferences/shared_preferences.dart';

import 'login_link.dart';

/// 一份留存的登录配置。字段与登录页的输入框一一对应。
class SavedLogin {
  const SavedLogin({
    required this.host,
    required this.port,
    required this.name,
    required this.password,
    this.token,
  });

  /// 服务器地址。可能是纯主机，也可能是骑手直接粘进来的完整 URL。
  final String host;

  /// 端口输入框里的文本（保留原始写法，不在这里解析）。
  final String port;

  final String name;
  final String password;

  /// 开发后门的令牌，留空 = 账号登录 / 观战。
  final String? token;

  bool get hasAccount => name.trim().isNotEmpty && password.isNotEmpty;
}

const _kHost = 'login.host';
const _kPort = 'login.port';
const _kName = 'login.name';
const _kPassword = 'login.password';
const _kToken = 'login.token';

/// 读上次的登录配置。没存过（或读不出来）返回 `null`。
Future<SavedLogin?> loadSavedLogin() async {
  try {
    final prefs = await SharedPreferences.getInstance();
    final host = prefs.getString(_kHost) ?? '';
    if (host.trim().isEmpty) return null;
    return SavedLogin(
      host: host,
      port: prefs.getString(_kPort) ?? '$kDefaultPort',
      name: prefs.getString(_kName) ?? '',
      password: prefs.getString(_kPassword) ?? '',
      token: prefs.getString(_kToken),
    );
  } catch (_) {
    // 本地存储坏了不该挡住登录：当成没存过。
    return null;
  }
}

/// 存一份配置。写失败也不抛——记住登录信息不是关键路径。
Future<void> saveLogin(SavedLogin login) async {
  try {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(_kHost, login.host);
    await prefs.setString(_kPort, login.port);
    await prefs.setString(_kName, login.name);
    await prefs.setString(_kPassword, login.password);
    if ((login.token ?? '').isEmpty) {
      await prefs.remove(_kToken);
    } else {
      await prefs.setString(_kToken, login.token!);
    }
  } catch (_) {
    // 忽略。
  }
}

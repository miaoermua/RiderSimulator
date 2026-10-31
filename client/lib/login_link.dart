/// 扫码登录连接串。
///
/// 内容形如
/// `ridersim://login?host=192.168.1.7&port=8080&name=%E5%BC%A0%E4%B8%89&password=abc123`，
/// 由烽火台（`server/console` 的「骑手接入」面板）生成，二维码里就是它。
///
/// 服务端权威实现见 `server/src/net/join.rs`——那边负责拼、这里负责解，
/// **两边必须一致**。改动任何一个都要同步另一个，并补 `packages/protocol.json`。
///
/// 注意：连接串里带**明文密码**。这是刻意的取舍（扫一下即登录），代价是这张码
/// 能截图、能打印、发出去收不回来；要作废只能让烽火台重置该骑手的密码。
library;

/// 自定义 scheme。
const String kLoginScheme = 'ridersim';

/// scheme 后面的固定主机名（`ridersim://login?...`）。
const String kLoginHost = 'login';

/// 服务端 WebSocket 固定挂载路径。
const String kWsPath = '/ws';

/// 默认端口，与服务端 `RIDER_SIM_ADDR` 缺省值一致。
const int kDefaultPort = 8080;

/// 一条登录连接串解析出来的结果。
class LoginLink {
  const LoginLink({
    required this.host,
    required this.port,
    required this.name,
    required this.password,
    this.url,
  });

  /// 手机可达的地址（IP 或域名），**不含端口**。
  final String host;
  final int port;

  /// 骑手名字 = 账号名。
  final String name;

  /// 明文密码。
  final String password;

  /// 服务端配了完整地址时带的那一条（`wss://game.example.com/ws`）。
  ///
  /// 反代后面协议、端口、路径都可能不是默认值，有它就不要再拼。
  final String? url;

  /// 该连的 WebSocket 地址。
  String get wsUrl => url ?? 'ws://$host:$port$kWsPath';

  /// 拼回连接串。
  ///
  /// 服务端 `JoinInfo::login_uri` 才是权威实现，这里只为单测自洽 / 本地调试。
  /// 用 `Uri.encodeComponent` 而不是 `encodeQueryComponent`：后者把空格编成 `+`，
  /// 服务端编的是 `%20`。两种都能被解回来，但没必要和它不一样。
  static String build({
    required String host,
    required int port,
    required String name,
    required String password,
    String? url,
  }) =>
      '$kLoginScheme://$kLoginHost'
      '?host=${Uri.encodeComponent(host)}'
      '&port=$port'
      '&name=${Uri.encodeComponent(name)}'
      '&password=${Uri.encodeComponent(password)}'
      '${url == null ? '' : '&url=${Uri.encodeComponent(url)}'}';

  /// 解析一条连接串。
  ///
  /// 不是我们的 scheme、缺字段、端口不合法都返回 `null`——**绝不抛异常**，
  /// 因为输入可能来自任何一张被随手扫到的二维码。
  static LoginLink? parse(String raw) {
    final text = raw.trim();
    if (text.isEmpty) return null;

    final uri = Uri.tryParse(text);
    if (uri == null) return null;
    if (uri.scheme.toLowerCase() != kLoginScheme) return null;

    // `ridersim://login` 里那个 `login` 算 host 还是算 path，取决于 Uri 怎么
    // 看待这个非标准 scheme。两边都认，免得依赖某个实现细节。
    final where = uri.host.isNotEmpty ? uri.host : uri.path;
    if (where.isNotEmpty && where.toLowerCase() != kLoginHost) return null;

    // 坏掉的百分号转义会让 queryParameters 抛：这时当作不是我们的码。
    final Map<String, String> q;
    try {
      q = uri.queryParameters;
    } on FormatException {
      return null;
    }

    final host = (q['host'] ?? '').trim();
    final name = (q['name'] ?? '').trim();
    final password = q['password'] ?? '';
    final port = int.tryParse((q['port'] ?? '').trim());

    if (host.isEmpty || name.isEmpty || password.isEmpty) return null;
    if (port == null || port <= 0 || port > 65535) return null;

    // 服务端配了完整地址时优先用它；写坏了就当没带，退回 host/port。
    final rawUrl = (q['url'] ?? '').trim();
    final url = rawUrl.isEmpty ? null : normalizeServerUrl(rawUrl);

    return LoginLink(
      host: host,
      port: port,
      name: name,
      password: password,
      url: (url == null || url.isEmpty) ? null : url,
    );
  }
}

/// 把用户随手填的服务器地址整理成 WebSocket 地址。
///
/// 接受这些写法，省得骑手对着输入框猜：
/// - `192.168.1.7`            → `ws://192.168.1.7:8080/ws`
/// - `192.168.1.7:9000`       → `ws://192.168.1.7:9000/ws`
/// - `ws://host:8080/ws`      → 原样
/// - `http://host:8080`       → `ws://host:8080/ws`
/// - `https://host`           → `wss://host:443/ws`
///
/// 认不出来（空、只有协议头）返回空串，调用方据此拦下。
String normalizeServerUrl(String raw, {int defaultPort = kDefaultPort}) {
  var text = raw.trim();
  if (text.isEmpty) return '';
  // 没有协议头就先补一个，好让 Uri 能解析出 host/port。
  if (!text.contains('://')) text = 'ws://$text';

  final uri = Uri.tryParse(text);
  if (uri == null || uri.host.isEmpty) return '';

  final scheme = switch (uri.scheme.toLowerCase()) {
    'wss' || 'https' => 'wss',
    // ws / http / 其它一律当明文 ws：局域网里没有证书。
    _ => 'ws',
  };
  final port = uri.hasPort ? uri.port : (scheme == 'wss' ? 443 : defaultPort);
  // 用户自己带了路径就用他的（可能挂在反代子路径下），否则用约定的 /ws。
  final path = (uri.path.isEmpty || uri.path == '/') ? kWsPath : uri.path;

  return '$scheme://${uri.host}:$port$path';
}

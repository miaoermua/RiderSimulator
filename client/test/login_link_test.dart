import 'package:flutter_test/flutter_test.dart';
import 'package:rider_client/login_link.dart';

/// 服务端 `server/src/net/join.rs` 的 `login_uri_carries_everything_the_form_needs`
/// 用的同一组值。**这是两边唯一的对接口**，所以这里不写「跟 build() 对得上」，
/// 而是直接钉死服务端会产出的那串字面量。
const _serverMadeUri =
    'ridersim://login?host=192.168.1.7&port=8080'
    '&name=%E5%BC%A0%E4%B8%89&password=abc123';

void main() {
  group('LoginLink.parse', () {
    test('认得服务端亲手拼出来的那串', () {
      final link = LoginLink.parse(_serverMadeUri);
      expect(link, isNotNull);
      expect(link!.host, '192.168.1.7');
      expect(link.port, 8080);
      expect(link.name, '张三', reason: '中文名字要能从百分号编码里还原');
      expect(link.password, 'abc123');
      expect(link.wsUrl, 'ws://192.168.1.7:8080/ws');
    });

    test('build 出来的能被自己解回来', () {
      final uri = LoginLink.build(
        host: '10.0.0.5',
        port: 9000,
        name: '李四',
        password: 'p@ss/w:rd?&=#',
      );
      final link = LoginLink.parse(uri);
      expect(link, isNotNull);
      expect(link!.host, '10.0.0.5');
      expect(link.port, 9000);
      expect(link.name, '李四');
      expect(link.password, 'p@ss/w:rd?&=#', reason: '密码里的查询串元字符要原样还原');
      expect(link.wsUrl, 'ws://10.0.0.5:9000/ws');
    });

    test('名字里有空格也能还原成空格', () {
      final link = LoginLink.parse(
        LoginLink.build(host: 'h', port: 1, name: '张 三', password: 'p'),
      );
      expect(link!.name, '张 三');
    });

    test('不是我们的码一律返回 null，且绝不抛', () {
      const garbage = [
        '',
        '   ',
        'https://example.com',
        'ridersim://login', // 一个参数都没有
        'ridersim://login?host=1.2.3.4&port=8080&name=x', // 缺密码
        'ridersim://login?host=1.2.3.4&port=abc&name=x&password=p',
        'ridersim://login?host=1.2.3.4&port=0&name=x&password=p',
        'ridersim://login?host=1.2.3.4&port=70000&name=x&password=p',
        'ridersim://login?host=&port=8080&name=x&password=p',
        'ridersim://login?host=1.2.3.4&port=8080&name=&password=p',
        'ridersim://login?host=1.2.3.4&port=8080&name=x&password=',
        'ridersim://other?host=1.2.3.4&port=8080&name=x&password=p',
      ];
      for (final raw in garbage) {
        expect(LoginLink.parse(raw), isNull, reason: '该拒：$raw');
      }
    });
  });

  group('normalizeServerUrl', () {
    test('裸地址补上默认端口与 /ws', () {
      expect(normalizeServerUrl('192.168.1.7'), 'ws://192.168.1.7:8080/ws');
      expect(normalizeServerUrl(' 192.168.1.7 '), 'ws://192.168.1.7:8080/ws');
    });

    test('带了端口就用它的', () {
      expect(normalizeServerUrl('192.168.1.7:9000'), 'ws://192.168.1.7:9000/ws');
    });

    test('http / https 换成 ws / wss', () {
      expect(normalizeServerUrl('http://host:8080'), 'ws://host:8080/ws');
      expect(normalizeServerUrl('https://host'), 'wss://host:443/ws');
    });

    test('已经是对的地址就原样留着', () {
      expect(normalizeServerUrl('ws://host:8080/ws'), 'ws://host:8080/ws');
    });

    test('反代挂在子路径下时保留路径', () {
      expect(normalizeServerUrl('wss://host/sim/ws'), 'wss://host:443/sim/ws');
    });

    test('认不出来就返回空串，让调用方拦下', () {
      expect(normalizeServerUrl(''), '');
      expect(normalizeServerUrl('   '), '');
      expect(normalizeServerUrl('ws://'), '');
    });
  });
}

import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import '../login_link.dart';

/// 扫码登录：对准烽火台给出的二维码。
///
/// 扫到的字符串**原样** `pop` 回登录页，解析交给 [LoginLink.parse]——
/// 这样「扫码」和「手动粘贴」共用同一条解析路径，只有一条要维护。
///
/// 刻意只用 `MobileScanner(onDetect:)` 这个最朴素的用法：不建 controller，
/// 就不必跟着 5.x / 6.x 的 controller 生命周期 API 走（差异很大）。
class ScanLoginPage extends StatefulWidget {
  const ScanLoginPage({super.key});

  @override
  State<ScanLoginPage> createState() => _ScanLoginPageState();
}

class _ScanLoginPageState extends State<ScanLoginPage> {
  /// 已经认出并准备返回了，忽略后面还在排队送上来的帧。
  bool _done = false;

  /// 最近一次扫到「不是我们的码」的提示。
  String? _hint;

  void _onDetect(BarcodeCapture capture) {
    if (_done) return;
    for (final barcode in capture.barcodes) {
      final raw = barcode.rawValue;
      if (raw == null || raw.trim().isEmpty) continue;

      if (LoginLink.parse(raw) == null) {
        // 扫到了但不对：提示一声，继续扫，别把页面关掉——骑手可能只是对着
        // 别的东西扫了一下。
        if (_hint == null) setState(() => _hint = '这不是骑手接入码，再对准一点');
        continue;
      }

      _done = true;
      Navigator.of(context).pop(raw);
      return;
    }
  }

  /// 相机用不了时的退路：把二维码内容粘进来。
  Future<void> _pasteInstead() async {
    final controller = TextEditingController();
    final text = await showDialog<String>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('粘贴登录串'),
        content: TextField(
          controller: controller,
          autofocus: true,
          minLines: 2,
          maxLines: 4,
          decoration: const InputDecoration(
            hintText: 'ridersim://login?host=…&port=…&name=…&password=…',
            border: OutlineInputBorder(),
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(ctx).pop(controller.text),
            child: const Text('登录'),
          ),
        ],
      ),
    );
    controller.dispose();

    if (!mounted || text == null || text.trim().isEmpty) return;
    Navigator.of(context).pop(text);
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('扫码登录'),
        actions: [
          TextButton(
            onPressed: _pasteInstead,
            child: const Text('粘贴'),
          ),
        ],
      ),
      body: Stack(
        fit: StackFit.expand,
        children: [
          MobileScanner(onDetect: _onDetect),
          // 取景框：纯粹为了对准，不参与识别。
          const _Viewfinder(),
          Positioned(
            left: 0,
            right: 0,
            bottom: 32,
            child: Column(
              children: [
                if (_hint != null)
                  Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 24),
                    child: Text(
                      _hint!,
                      textAlign: TextAlign.center,
                      style: const TextStyle(color: Colors.white, fontSize: 15),
                    ),
                  ),
                const SizedBox(height: 8),
                const Text(
                  '对准烽火台给出的骑手接入码',
                  textAlign: TextAlign.center,
                  style: TextStyle(color: Colors.white70),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// 屏幕中间那个白色缺口框。
class _Viewfinder extends StatelessWidget {
  const _Viewfinder();

  @override
  Widget build(BuildContext context) {
    return IgnorePointer(
      child: Center(
        child: Container(
          width: 240,
          height: 240,
          decoration: BoxDecoration(
            border: Border.all(color: Colors.white, width: 3),
            borderRadius: BorderRadius.circular(12),
          ),
        ),
      ),
    );
  }
}

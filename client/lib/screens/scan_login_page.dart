import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import '../login_link.dart';

/// 扫码登录：对准烽火台给出的二维码。
///
/// 扫到的字符串**原样** `pop` 回登录页，解析交给 [LoginLink.parse]——
/// 这样「扫码」和「手动粘贴」共用同一条解析路径，只有一条要维护。
///
/// 相机是原生能力，也是最容易翻车的一环：权限被拒、机型不支持、CameraX
/// 起不来，都会让预览变成一块黑。所以这里**自己持有 controller**，并且
/// 给 `errorBuilder` 配了中文解释和两条退路（重试 / 粘贴），
/// 而不是让插件默认的英文错误页甩在骑手脸上。
class ScanLoginPage extends StatefulWidget {
  const ScanLoginPage({super.key});

  @override
  State<ScanLoginPage> createState() => _ScanLoginPageState();
}

class _ScanLoginPageState extends State<ScanLoginPage> {
  late final MobileScannerController _controller;

  /// 已经认出并准备返回了，忽略后面还在排队送上来的帧。
  bool _done = false;

  /// 最近一次扫到「不是我们的码」的提示。
  String? _hint;

  @override
  void initState() {
    super.initState();
    _controller = MobileScannerController(
      // 只认二维码：别的码扫进来也是白搭，少一类误报。
      formats: const [BarcodeFormat.qrCode],
      // 同一张码连续送帧时别反复触发。
      detectionSpeed: DetectionSpeed.noDuplicates,
    );
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

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

  /// 相机起不来时再给一次机会。`stop` 失败没关系，`start` 成功就行。
  Future<void> _retryCamera() async {
    try {
      await _controller.stop();
    } catch (_) {
      // 本来就是停着的 / 已经释放，忽略。
    }
    try {
      await _controller.start();
    } catch (_) {
      // 还是不行，errorBuilder 会重新画一遍错误页。
    }
    if (mounted) setState(() => _hint = null);
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
      backgroundColor: Colors.black,
      appBar: AppBar(
        backgroundColor: Colors.black,
        foregroundColor: Colors.white,
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
          MobileScanner(
            controller: _controller,
            onDetect: _onDetect,
            // 插件默认的英文错误页（"An unexpected error occurred."）对骑手
            // 毫无用处，这里换成「说人话 + 给出路」。
            errorBuilder: (context, error) => _CameraError(
              error: error,
              onRetry: _retryCamera,
              onPaste: _pasteInstead,
            ),
          ),
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

/// 相机起不来时整页顶替预览的那块。
class _CameraError extends StatelessWidget {
  const _CameraError({
    required this.error,
    required this.onRetry,
    required this.onPaste,
  });

  final Object error;
  final Future<void> Function() onRetry;
  final Future<void> Function() onPaste;

  @override
  Widget build(BuildContext context) {
    final (title, detail) = _explain(error);
    return Container(
      color: Colors.black,
      padding: const EdgeInsets.all(28),
      child: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.no_photography_outlined,
                size: 64, color: Colors.white54),
            const SizedBox(height: 16),
            Text(
              title,
              textAlign: TextAlign.center,
              style: const TextStyle(color: Colors.white, fontSize: 18),
            ),
            const SizedBox(height: 10),
            Text(
              detail,
              textAlign: TextAlign.center,
              style: const TextStyle(
                  color: Colors.white70, fontSize: 13, height: 1.5),
            ),
            const SizedBox(height: 24),
            Wrap(
              spacing: 12,
              runSpacing: 8,
              alignment: WrapAlignment.center,
              children: [
                FilledButton.icon(
                  onPressed: onRetry,
                  icon: const Icon(Icons.refresh, size: 18),
                  label: const Text('再试一次'),
                ),
                TextButton.icon(
                  onPressed: onPaste,
                  icon: const Icon(Icons.content_paste,
                      size: 18, color: Colors.white70),
                  label: const Text(
                    '改用粘贴登录串',
                    style: TextStyle(color: Colors.white70),
                  ),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  /// 把插件的英文错误码翻成一句人话 + 一句下一步。
  static (String, String) _explain(Object error) {
    if (error is! MobileScannerException) {
      return ('相机起不来', '原因：$error\n可以直接粘贴登录串进来。');
    }
    final raw = error.errorDetails?.message;
    switch (error.errorCode) {
      case MobileScannerErrorCode.permissionDenied:
        return (
          '没有相机权限',
          '到系统设置里给「骑手模拟器」打开相机权限，再回来点「再试一次」。\n'
              '不想折腾的话，粘贴登录串一样能进。',
        );
      case MobileScannerErrorCode.unsupported:
        return (
          '这台设备不支持扫码',
          '相机模块不可用（模拟器、部分平板常见）。用「粘贴」把烽火台上的登录串贴进来。',
        );
      case MobileScannerErrorCode.genericError:
        return (
          '相机启动失败',
          '相机被别的应用占着，或者系统相机服务没响应。关掉相机类应用再试一次；'
              '还不行就用粘贴登录。${raw == null ? '' : '\n（$raw）'}',
        );
      default:
        return (
          '相机现在用不了',
          '${error.errorCode.name}${raw == null ? '' : ' · $raw'}'
        );
    }
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

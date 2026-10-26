import 'package:file_picker/file_picker.dart';

import 'alert_event.dart';

/// 浏览器里没有可持久化的应用私有目录，自定义音效不提供。
///
/// 设置页据 [AlertCenter.canPickCustomSound] 隐藏入口，这里只是兜底。
Future<String?> saveSoundFile(AlertSoundSlot slot, PlatformFile file) async =>
    null;

Future<void> deleteSoundFile(String? path) async {}

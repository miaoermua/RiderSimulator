import 'dart:io';

import 'package:file_picker/file_picker.dart';
import 'package:path_provider/path_provider.dart';

import 'alert_event.dart';

/// 把用户选的音频复制进应用私有目录，返回新路径。
///
/// **必须复制**：file_picker 在 Android 上给的是缓存文件（系统随时可清），
/// iOS 上还可能是 SAF 授权出来的临时副本。记住原路径等于下次开机就失效。
Future<String?> saveSoundFile(AlertSoundSlot slot, PlatformFile file) async {
  final base = await getApplicationSupportDirectory();
  final folder = Directory('${base.path}/alert_sounds');
  await folder.create(recursive: true);

  // 换过格式时旧文件扩展名不同，先按槽位清一遍同名残留。
  final ext = (file.extension ?? 'mp3').toLowerCase();
  final target = File('${folder.path}/${slot.name}.$ext');
  if (await target.exists()) await target.delete();
  await target.writeAsBytes(await file.readAsBytes(), flush: true);
  return target.path;
}

Future<void> deleteSoundFile(String? path) async {
  if (path == null || path.isEmpty) return;
  try {
    final f = File(path);
    if (await f.exists()) await f.delete();
  } catch (_) {
    // 删不掉不影响使用：下次换音效会覆盖同名文件。
  }
}

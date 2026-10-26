// 自定义音效的落盘实现。
//
// Web 没有应用私有目录，`dart:io` 也用不了——条件导入把两边分开，
// 这样 `flutter build web` 不会因为一个设置项编译不过。
export 'alert_sound_store_io.dart'
    if (dart.library.html) 'alert_sound_store_web.dart';

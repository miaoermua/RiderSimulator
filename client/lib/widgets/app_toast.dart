import 'package:flutter/material.dart';
import 'package:fluttertoast/fluttertoast.dart';

/// 全局轻提示：走系统原生 Toast。
///
/// Android 上是真的 `android.widget.Toast`（叠在应用之上、不占布局），
/// iOS 上是 Toast 库的浮起小条，Web 上是 toastify。
/// 与 `SnackBar` 的区别就在这：SnackBar 是页面里的一条横幅，会挤压布局、
/// 还会顶掉上一条；Toast 是系统级提示，骑手点完按钮立刻能看到反馈。
///
/// 失败用它、成功也用它——`bad` 只换底色，不换位置，
/// 免得同一个位置一会儿红一会儿灰。
void showAppToast(String msg, {bool bad = false}) {
  // 连续失败时后一条直接顶掉前一条，不排队。
  Fluttertoast.cancel();
  Fluttertoast.showToast(
    msg: msg,
    toastLength: Toast.LENGTH_SHORT,
    gravity: ToastGravity.BOTTOM,
    timeInSecForIosWeb: 2,
    backgroundColor: bad ? const Color(0xFFB3261E) : const Color(0xE6323232),
    textColor: Colors.white,
    fontSize: 14,
  );
}

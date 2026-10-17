import 'package:flutter/material.dart';

/// 空态：一张插画 + 原来说明文字。
///
/// 只负责「摆」，文案由调用方给——活动页那句「当前没有进行中的活动。」、
/// 对话面板那两句提示都是各自的业务话术，不往这里搬。
///
/// 图是外部素材（线稿图标，透明底），不跟主题变色；外面套一层滚动，
/// 免得键盘弹起或小屏上「图 + 三行字」溢出。
class EmptyStateArt extends StatelessWidget {
  const EmptyStateArt({
    super.key,
    required this.asset,
    required this.message,
    this.size = 140,
  });

  /// 图片资源路径，如 `assets/illustrations/box.png`。
  final String asset;

  /// 原来的说明文字，保持原样显示。
  final String message;

  /// 插画边长。位置窄（对话面板）就传小一点。
  final double size;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Image.asset(
              asset,
              width: size,
              height: size,
              filterQuality: FilterQuality.medium,
            ),
            const SizedBox(height: 12),
            Text(
              message,
              textAlign: TextAlign.center,
              style: const TextStyle(color: Colors.grey),
            ),
          ],
        ),
      ),
    );
  }
}

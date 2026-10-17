import 'package:flutter/material.dart';

/// 滑动确认：按住滑块拖到最右才算数。
///
/// 抢单和送达都是一次性、不可撤销的动作（抢到了就要跑，送达了就结单），
/// 手指一抖点错没有后悔药。滑动的成本刚好够把人从「顺手点」拦下来，
/// 又比长按 / 二次弹窗少一次决策。
///
/// 不引第三方包：控件本身只有一条轨道加一颗滑块，
/// 依赖越少，真机上出问题的面越小。
class SlideToConfirm extends StatefulWidget {
  const SlideToConfirm({
    super.key,
    required this.label,
    required this.onConfirm,
    this.icon = Icons.arrow_forward,
    this.enabled = true,
    this.height = 44,
    this.color,
  });

  /// 轨道上的字，例如「滑动抢单」。
  final String label;

  /// 拖到右端松手时触发。只会触发一次。
  final VoidCallback onConfirm;

  final IconData icon;
  final bool enabled;
  final double height;

  /// 默认取主题主色；送达这类「完成」动作可以传绿色。
  final Color? color;

  @override
  State<SlideToConfirm> createState() => _SlideToConfirmState();
}

class _SlideToConfirmState extends State<SlideToConfirm>
    with SingleTickerProviderStateMixin {
  /// 0 = 停在左端，1 = 顶到右端。拖动和回弹都走同一个控制器。
  late final AnimationController _pos = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 180),
  );

  /// 轨道可滑动的像素长度，布局时才知道。
  double _maxDx = 0;

  /// 已经确认过，不再接受拖动。
  bool _fired = false;

  bool get _usable => widget.enabled && !_fired;

  @override
  void dispose() {
    _pos.dispose();
    super.dispose();
  }

  void _drag(double dx) {
    if (!_usable || _maxDx <= 0) return;
    final next = _pos.value * _maxDx + dx;
    _pos.value = (next / _maxDx).clamp(0.0, 1.0);
  }

  void _release() {
    if (!_usable) return;
    // 不要求真的顶到边：手指到 90% 就算数，免得差几像素反复失败。
    if (_pos.value >= 0.9) {
      _fired = true;
      _pos.animateTo(1, duration: const Duration(milliseconds: 80));
      widget.onConfirm();
    } else {
      _pos.animateTo(0, curve: Curves.easeOut);
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final color = widget.color ?? scheme.primary;
    return LayoutBuilder(
      builder: (context, constraints) {
        final h = widget.height;
        final knob = h - 8;
        _maxDx = (constraints.maxWidth - knob - 8).clamp(0.0, double.infinity);
        return AnimatedBuilder(
          animation: _pos,
          builder: (context, _) {
            final dx = _pos.value * _maxDx;
            final done = _pos.value >= 0.999;
            return Opacity(
              opacity: widget.enabled ? 1 : 0.4,
              child: Container(
                height: h,
                width: double.infinity,
                decoration: BoxDecoration(
                  color: color.withValues(alpha: 0.12),
                  borderRadius: BorderRadius.circular(h / 2),
                  border: Border.all(color: color.withValues(alpha: 0.45)),
                ),
                child: Stack(
                  alignment: Alignment.centerLeft,
                  children: [
                    // 文案随滑块淡出：拖起来之后它就是噪音。
                    Center(
                      child: Opacity(
                        opacity: (1 - _pos.value).clamp(0.0, 1.0),
                        child: Text(
                          widget.label,
                          style: TextStyle(
                            color: color,
                            fontSize: 13,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                    ),
                    Positioned(
                      left: 4 + dx,
                      top: 4,
                      child: GestureDetector(
                        behavior: HitTestBehavior.opaque,
                        onHorizontalDragUpdate: (d) => _drag(d.delta.dx),
                        onHorizontalDragEnd: (_) => _release(),
                        onHorizontalDragCancel: _release,
                        child: Container(
                          width: knob,
                          height: knob,
                          decoration: BoxDecoration(
                            color: color,
                            shape: BoxShape.circle,
                            boxShadow: const [
                              BoxShadow(
                                color: Colors.black26,
                                blurRadius: 4,
                                offset: Offset(0, 1),
                              ),
                            ],
                          ),
                          child: Icon(
                            done ? Icons.check : widget.icon,
                            size: 18,
                            color: Colors.white,
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            );
          },
        );
      },
    );
  }
}

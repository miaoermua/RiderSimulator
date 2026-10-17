import 'package:flutter/material.dart';

import '../game_state.dart';
import '../protocol.dart';

/// 点开首页通知：看全文；关联了活动就顺带展示「我的进度」。
///
/// 通知只有一条（烽火台改了就覆盖），所以这里没有列表，也没有「已读」概念。
void showNoticeSheet(
  BuildContext context, {
  required GameState state,
  required NoticeView notice,
}) {
  showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (_) => _NoticeSheet(state: state, notice: notice),
  );
}

class _NoticeSheet extends StatelessWidget {
  const _NoticeSheet({required this.state, required this.notice});

  final GameState state;
  final NoticeView notice;

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: state,
      builder: (context, _) {
        final activity = notice.activityId == null
            ? null
            : _findActivity(notice.activityId!);
        return SafeArea(
          child: ConstrainedBox(
            constraints: BoxConstraints(
              maxHeight: MediaQuery.of(context).size.height * 0.75,
            ),
            child: SingleChildScrollView(
              padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text(
                    notice.title,
                    style: Theme.of(context).textTheme.titleLarge,
                  ),
                  const SizedBox(height: 4),
                  Text(
                    _postedAt(),
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                  const SizedBox(height: 12),
                  SelectableText(
                    notice.body,
                    style: const TextStyle(fontSize: 15, height: 1.5),
                  ),
                  if (activity != null) ...[
                    const SizedBox(height: 16),
                    const Divider(height: 1),
                    const SizedBox(height: 12),
                    _ActivityBlock(
                      activity: activity,
                      progress: state.me?.progressOf(activity.id),
                      isRider: state.me != null,
                    ),
                  ] else if (notice.activityId != null) ...[
                    const SizedBox(height: 16),
                    const Divider(height: 1),
                    const SizedBox(height: 12),
                    Text(
                      '这条通知关联的活动已经不在了。',
                      style: TextStyle(color: Colors.grey.shade600),
                    ),
                  ],
                ],
              ),
            ),
          ),
        );
      },
    );
  }

  ActivityView? _findActivity(int id) {
    for (final a in state.activities) {
      if (a.id == id) return a;
    }
    return null;
  }

  /// 「第 N 天 HH:MM 发布」。时间以模拟时钟为准，别拿手机时间糊弄。
  String _postedAt() {
    final ago = (state.elapsedMin - notice.updatedAtMin).clamp(0, 1 << 30);
    final at = (state.simMinute - ago).clamp(0, 1 << 30);
    final hh = (at % 1440 ~/ 60).toString().padLeft(2, '0');
    final mm = (at % 60).toString().padLeft(2, '0');
    return '第 ${at ~/ 1440 + 1} 天 $hh:$mm 发布';
  }
}

class _ActivityBlock extends StatelessWidget {
  const _ActivityBlock({
    required this.activity,
    required this.progress,
    required this.isRider,
  });

  final ActivityView activity;
  final ActivityProgressView? progress;
  final bool isRider;

  @override
  Widget build(BuildContext context) {
    final done = progress?.completed ?? 0;
    final target = activity.targetOrders;
    final rewarded = progress?.rewarded ?? false;
    final remain = target - done;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            const Icon(Icons.emoji_events_outlined, size: 18),
            const SizedBox(width: 6),
            Expanded(
              child: Text(
                activity.title,
                style: const TextStyle(fontWeight: FontWeight.w600),
              ),
            ),
            if (!activity.active)
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                decoration: BoxDecoration(
                  color: Colors.grey.shade300,
                  borderRadius: BorderRadius.circular(4),
                ),
                child: const Text('已下架', style: TextStyle(fontSize: 11)),
              ),
          ],
        ),
        const SizedBox(height: 4),
        Text(
          '目标 $target 单 · 奖金 ${yuan(activity.rewardCents)} 元',
          style: TextStyle(fontSize: 12, color: Colors.grey.shade700),
        ),
        if (isRider) ...[
          const SizedBox(height: 10),
          ClipRRect(
            borderRadius: BorderRadius.circular(4),
            child: LinearProgressIndicator(
              value: target <= 0 ? 0 : (done / target).clamp(0, 1),
              minHeight: 6,
            ),
          ),
          const SizedBox(height: 6),
          Text(
            rewarded
                ? '已跑 $done/$target 单 · 奖金已发放'
                : '已跑 $done/$target 单'
                    '${remain > 0 ? ' · 还差 $remain 单' : ' · 已达标'}',
            style: const TextStyle(fontSize: 12),
          ),
        ],
      ],
    );
  }
}

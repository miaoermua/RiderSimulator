import 'package:flutter/material.dart';

import '../game_state.dart';
import '../protocol.dart';

/// 通知和活动中心：一页两签。
///
/// * **活动** —— 烽火台配的平台活动（跑够 N 单给多少钱），带自己的进度。
/// * **日志** —— 原来那一整页通知（新单、财务、罚款、对话、系统提示），
///   内容没变，只是改叫「日志」：它本来就是按时间倒序的流水。
///
/// 顶栏铃铛的红点数的是日志未读数，进来就算读过。
class NotificationsPage extends StatefulWidget {
  const NotificationsPage({super.key, required this.state});

  final GameState state;

  @override
  State<NotificationsPage> createState() => _NotificationsPageState();
}

class _NotificationsPageState extends State<NotificationsPage>
    with SingleTickerProviderStateMixin {
  late final TabController _tabs = TabController(length: 2, vsync: this);

  @override
  void initState() {
    super.initState();
    // 进来就算读过：红点在这里清掉。
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) widget.state.markLogsRead();
    });
  }

  @override
  void dispose() {
    _tabs.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('通知和活动中心'),
        bottom: TabBar(
          controller: _tabs,
          tabs: const [
            Tab(text: '活动'),
            Tab(text: '日志'),
          ],
        ),
        actions: [
          IconButton(
            tooltip: '全部已读',
            onPressed: widget.state.markLogsRead,
            icon: const Icon(Icons.done_all),
          ),
        ],
      ),
      body: AnimatedBuilder(
        animation: widget.state,
        builder: (context, _) => TabBarView(
          controller: _tabs,
          children: [
            _ActivitiesTab(state: widget.state),
            _LogsTab(state: widget.state),
          ],
        ),
      ),
    );
  }
}

/// 活动页：只列进行中的活动。
class _ActivitiesTab extends StatelessWidget {
  const _ActivitiesTab({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    final running = state.runningActivities;
    if (running.isEmpty) {
      return const Center(
        child: Text(
          '当前没有进行中的活动。',
          style: TextStyle(color: Colors.grey),
        ),
      );
    }
    return ListView.separated(
      padding: const EdgeInsets.all(12),
      itemCount: running.length,
      separatorBuilder: (_, __) => const SizedBox(height: 10),
      itemBuilder: (context, i) => _ActivityCard(
        activity: running[i],
        progress: state.me?.progressOf(running[i].id),
      ),
    );
  }
}

class _ActivityCard extends StatelessWidget {
  const _ActivityCard({required this.activity, required this.progress});

  final ActivityView activity;

  /// `null` = 观战者，看不到「我的进度」。
  final ActivityProgressView? progress;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final rewarded = progress?.rewarded ?? false;
    final completed = progress?.completed ?? 0;
    final target = activity.targetOrders;
    final ratio = target <= 0 ? 1.0 : (completed / target).clamp(0.0, 1.0);
    final left = target - completed;

    return Card(
      margin: EdgeInsets.zero,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(14, 12, 14, 14),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text(
                    activity.title,
                    style: theme.textTheme.titleMedium
                        ?.copyWith(fontWeight: FontWeight.w600),
                  ),
                ),
                Text(
                  '+${yuan(activity.rewardCents)} 元',
                  style: theme.textTheme.titleMedium?.copyWith(
                    color: Colors.teal,
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ],
            ),
            const SizedBox(height: 4),
            Text(
              target == 1 ? '活动期间完成 1 单' : '活动期间完成 $target 单',
              style: theme.textTheme.bodySmall?.copyWith(color: Colors.grey),
            ),
            if (progress != null) ...[
              const SizedBox(height: 10),
              ClipRRect(
                borderRadius: BorderRadius.circular(4),
                child: LinearProgressIndicator(
                  value: ratio,
                  minHeight: 6,
                  backgroundColor: Colors.grey.withValues(alpha: 0.2),
                  color: rewarded ? Colors.teal : theme.colorScheme.primary,
                ),
              ),
              const SizedBox(height: 8),
              Row(
                children: [
                  Text(
                    '我的进度 $completed/$target 单',
                    style: theme.textTheme.bodySmall,
                  ),
                  const Spacer(),
                  if (rewarded)
                    const Text(
                      '奖金已到账',
                      style: TextStyle(color: Colors.teal, fontSize: 12),
                    )
                  else
                    Text(
                      left > 0 ? '还差 $left 单' : '已达标，下一单入账',
                      style: const TextStyle(color: Colors.orange, fontSize: 12),
                    ),
                ],
              ),
            ],
          ],
        ),
      ),
    );
  }
}

/// 日志页：原来那一整页通知。
class _LogsTab extends StatelessWidget {
  const _LogsTab({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    final logs = state.logs;
    if (logs.isEmpty) {
      return const Center(
        child: Text('还没有日志。', style: TextStyle(color: Colors.grey)),
      );
    }
    return ListView.separated(
      itemCount: logs.length,
      separatorBuilder: (_, __) => const Divider(height: 1),
      itemBuilder: (context, i) {
        final line = logs[i];
        return ListTile(
          dense: true,
          leading: Icon(logIcon(line.kind), size: 20, color: logColor(line.kind)),
          title: Text(line.text),
          subtitle: Text(
            clockLabel(line.at.hour * 60 + line.at.minute),
            style: const TextStyle(fontSize: 11),
          ),
        );
      },
    );
  }
}

IconData logIcon(LogKind k) => switch (k) {
      LogKind.good => Icons.check_circle_outline,
      LogKind.bad => Icons.error_outline,
      LogKind.money => Icons.paid_outlined,
      LogKind.chat => Icons.chat_bubble_outline,
      LogKind.info => Icons.info_outline,
    };

Color logColor(LogKind k) => switch (k) {
      LogKind.good => Colors.green,
      LogKind.bad => Colors.red,
      LogKind.money => Colors.teal,
      LogKind.chat => Colors.indigo,
      LogKind.info => Colors.grey,
    };

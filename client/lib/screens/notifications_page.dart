import 'package:flutter/material.dart';

import '../game_state.dart';
import '../protocol.dart';

/// 通知消息：新单、财务、罚款、对话、系统提示。
///
/// 就是 [GameState.logs] 的全量视图——顶栏铃铛红点数的也是它。
class NotificationsPage extends StatefulWidget {
  const NotificationsPage({super.key, required this.state});

  final GameState state;

  @override
  State<NotificationsPage> createState() => _NotificationsPageState();
}

class _NotificationsPageState extends State<NotificationsPage> {
  @override
  void initState() {
    super.initState();
    // 进来就算读过：红点在这里清掉。
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) widget.state.markLogsRead();
    });
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('通知消息'),
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
        builder: (context, _) {
          final logs = widget.state.logs;
          if (logs.isEmpty) {
            return const Center(
              child: Text('还没有通知。', style: TextStyle(color: Colors.grey)),
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
        },
      ),
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

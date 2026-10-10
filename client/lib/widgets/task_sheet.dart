import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';

/// 附加任务的对话面板。任务本身由服务端判定，这里只负责把话说出去。
void showTaskSheet(
  BuildContext context, {
  required OrderView order,
  required GameState state,
  required GameClient client,
}) {
  showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    builder: (_) => _TaskSheet(
      orderId: order.id,
      taskName: order.task?.name ?? '附加任务',
      state: state,
      client: client,
    ),
  );
}

class _TaskSheet extends StatefulWidget {
  const _TaskSheet({
    required this.orderId,
    required this.taskName,
    required this.state,
    required this.client,
  });

  final int orderId;
  final String taskName;
  final GameState state;
  final GameClient client;

  @override
  State<_TaskSheet> createState() => _TaskSheetState();
}

class _TaskSheetState extends State<_TaskSheet> {
  final _input = TextEditingController();

  @override
  void dispose() {
    _input.dispose();
    super.dispose();
  }

  void _send() {
    final text = _input.text.trim();
    if (text.isEmpty) return;
    widget.client.sendTaskMessage(widget.orderId, text);
    _input.clear();
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: EdgeInsets.only(
        bottom: MediaQuery.of(context).viewInsets.bottom,
      ),
      child: AnimatedBuilder(
        animation: widget.state,
        builder: (context, _) {
          final lines = widget.state.taskLines[widget.orderId] ?? const <TaskLine>[];
          return SizedBox(
            height: MediaQuery.of(context).size.height * 0.5,
            child: Column(
              children: [
                Padding(
                  padding: const EdgeInsets.all(12),
                  child: Text(
                    widget.taskName,
                    style: Theme.of(context).textTheme.titleMedium,
                  ),
                ),
                const Divider(height: 1),
                Expanded(
                  child: lines.isEmpty
                      ? const Center(
                          child: Padding(
                            padding: EdgeInsets.all(24),
                            child: Text(
                              '发一句话开始任务对话，例如「放门口吧」。',
                              style: TextStyle(color: Colors.grey),
                            ),
                          ),
                        )
                      : ListView.builder(
                          padding: const EdgeInsets.all(12),
                          itemCount: lines.length,
                          itemBuilder: (context, i) => _Bubble(line: lines[i]),
                        ),
                ),
                SafeArea(
                  child: Padding(
                    padding: const EdgeInsets.all(8),
                    child: Row(
                      children: [
                        Expanded(
                          child: TextField(
                            controller: _input,
                            textInputAction: TextInputAction.send,
                            onSubmitted: (_) => _send(),
                            decoration: const InputDecoration(
                              hintText: '对顾客说点什么…',
                              border: OutlineInputBorder(),
                              isDense: true,
                            ),
                          ),
                        ),
                        const SizedBox(width: 8),
                        IconButton.filled(
                          onPressed: _send,
                          icon: const Icon(Icons.send),
                        ),
                      ],
                    ),
                  ),
                ),
              ],
            ),
          );
        },
      ),
    );
  }
}

class _Bubble extends StatelessWidget {
  const _Bubble({required this.line});

  final TaskLine line;

  @override
  Widget build(BuildContext context) {
    final align = line.isMe ? Alignment.centerRight : Alignment.centerLeft;
    final color = switch (line.from) {
      'rider' => Colors.green.shade100,
      'system' => Colors.blueGrey.shade100,
      _ => Colors.grey.shade200,
    };
    final who = switch (line.from) {
      'rider' => '我',
      'system' => '系统',
      _ => '顾客',
    };
    return Align(
      alignment: align,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 3),
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
        constraints: const BoxConstraints(maxWidth: 280),
        decoration: BoxDecoration(
          color: color,
          borderRadius: BorderRadius.circular(10),
        ),
        child: Text('$who：${line.text}'),
      ),
    );
  }
}

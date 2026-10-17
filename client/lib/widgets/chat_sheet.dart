import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';

/// 「在线对话」面板：末端会话 + 附加任务对话，混成一条时间线。
///
/// 服务端把两者分成两条通道——任务对话要判定进度，末端闲聊不判定。但骑手
/// 眼里只有「跟这单的人说话」，所以这里合成一条，任务消息打「任务」标记。
///
/// 输入框自动选通道：有未完成的任务 → [GameClient.sendTaskMessage]（会判定），
/// 否则 → [GameClient.sendChatSay]。一条时间线只能配一个输入框，这就是取舍。
///
/// 顾客会不会回应由服务端决定（大多数单子不会有人开口），所以这里刻意不
/// 承诺「必有回音」——空列表是常态，不是 bug。
void showChatSheet(
  BuildContext context, {
  required OrderView order,
  required GameState state,
  required GameClient client,
}) {
  showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    builder: (_) => _ChatSheet(
      orderId: order.id,
      customerName: order.customerName,
      state: state,
      client: client,
    ),
  );
}

class _ChatSheet extends StatefulWidget {
  const _ChatSheet({
    required this.orderId,
    required this.customerName,
    required this.state,
    required this.client,
  });

  final int orderId;
  final String customerName;
  final GameState state;
  final GameClient client;

  @override
  State<_ChatSheet> createState() => _ChatSheetState();
}

class _ChatSheetState extends State<_ChatSheet> {
  final _input = TextEditingController();

  @override
  void dispose() {
    _input.dispose();
    super.dispose();
  }

  /// 这单当前是否还有「在办」的附加任务。有就必须走任务通道，否则说了不算。
  ExtraTaskView? get _pendingTask {
    final task = widget.state.orderById(widget.orderId)?.task;
    if (task == null || task.done || task.failed) return null;
    return task;
  }

  void _send() {
    final text = _input.text.trim();
    if (text.isEmpty) return;
    final task = _pendingTask;
    if (task != null) {
      widget.client.sendTaskMessage(widget.orderId, text);
    } else {
      widget.client.sendChatSay(widget.orderId, text);
    }
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
          final lines = widget.state.timelineFor(widget.orderId);
          final task = _pendingTask;
          return SizedBox(
            height: MediaQuery.of(context).size.height * 0.6,
            child: Column(
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(12, 12, 12, 8),
                  child: Row(
                    children: [
                      Expanded(
                        child: Text(
                          '在线对话 · ${widget.customerName}',
                          style: Theme.of(context).textTheme.titleMedium,
                        ),
                      ),
                      // 时效提示：对话只留一天。别让骑手以为能翻旧账。
                      Text(
                        '仅保留一天',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                    ],
                  ),
                ),
                if (task != null)
                  Container(
                    width: double.infinity,
                    margin: const EdgeInsets.fromLTRB(12, 0, 12, 8),
                    padding:
                        const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
                    decoration: BoxDecoration(
                      color: Colors.orange.shade50,
                      borderRadius: BorderRadius.circular(8),
                    ),
                    child: Text(
                      '任务：${task.name} · 奖励 ${yuan(task.rewardCents)} 元'
                      '${task.penaltyCents > 0 ? ' · 做错扣 ${yuan(task.penaltyCents)} 元' : ''}\n'
                      '现在说的话会判定任务进度；说完任务再看普通会话。',
                      style: TextStyle(fontSize: 12, color: Colors.orange.shade900),
                    ),
                  ),
                const Divider(height: 1),
                Expanded(
                  child: lines.isEmpty
                      ? Center(
                          child: Padding(
                            padding: const EdgeInsets.all(24),
                            child: Text(
                              task != null
                                  ? '还没开始聊。\n说一句试试，例如「给你放门口行吗」——'
                                      '这句话会直接判定任务。'
                                  : '顾客还没开口。\n说一句试试——不过大多数单子不会有人回。',
                              textAlign: TextAlign.center,
                              style: const TextStyle(color: Colors.grey),
                            ),
                          ),
                        )
                      : ListView.builder(
                          padding: const EdgeInsets.all(12),
                          itemCount: lines.length,
                          itemBuilder: (context, i) => _Bubble(entry: lines[i]),
                        ),
                ),
                // 预设话术：不想打字时一键发。**文本由服务端补全**，
                // 这里只发枚举名，所以「骑手说了什么」永远可审计。
                // 任务在办时不显示：预设走的是普通会话，不会推进任务，给了会误导。
                if (task == null)
                  SizedBox(
                    height: 40,
                    child: ListView.separated(
                      scrollDirection: Axis.horizontal,
                      padding: const EdgeInsets.symmetric(horizontal: 8),
                      itemCount: ChatPreset.values.length,
                      separatorBuilder: (_, __) => const SizedBox(width: 6),
                      itemBuilder: (context, i) {
                        final p = ChatPreset.values[i];
                        return ActionChip(
                          label: Text(p.label),
                          onPressed: () =>
                              widget.client.sendChatPreset(widget.orderId, p),
                        );
                      },
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
                            decoration: InputDecoration(
                              hintText: task != null
                                  ? '说明这单任务怎么交付…（会判定）'
                                  : '对顾客说点什么…',
                              border: const OutlineInputBorder(),
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
  const _Bubble({required this.entry});

  final ChatEntry entry;

  @override
  Widget build(BuildContext context) {
    final align = entry.isMe ? Alignment.centerRight : Alignment.centerLeft;
    final color = switch (entry.from) {
      'rider' => Colors.green.shade100,
      'system' => Colors.blueGrey.shade100,
      // 带赔付的顾客消息标红：这不是情绪，是**钱**。
      'customer' when entry.isCostly => Colors.red.shade100,
      'customer' when entry.isTask => Colors.orange.shade100,
      _ => Colors.grey.shade200,
    };
    final who = switch (entry.from) {
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
        child: Column(
          crossAxisAlignment:
              entry.isMe ? CrossAxisAlignment.end : CrossAxisAlignment.start,
          children: [
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                if (entry.isTask) ...[
                  Container(
                    padding:
                        const EdgeInsets.symmetric(horizontal: 4, vertical: 1),
                    decoration: BoxDecoration(
                      color: Colors.orange.shade700,
                      borderRadius: BorderRadius.circular(3),
                    ),
                    child: const Text(
                      '任务',
                      style: TextStyle(fontSize: 9, color: Colors.white),
                    ),
                  ),
                  const SizedBox(width: 4),
                ],
                Flexible(child: Text('$who：${entry.text}')),
              ],
            ),
            const SizedBox(height: 2),
            Text(
              entry.isCostly
                  ? '${entry.at} · 赔付 ${yuan(entry.compensationCents)} 元'
                  : entry.at,
              style: TextStyle(
                fontSize: 11,
                color: entry.isCostly ? Colors.red.shade900 : Colors.black54,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';

/// 订单会话面板（末端交付沟通）。
///
/// 与「任务对话」是两回事：那边绑在附加任务上，没挂任务的单会被服务端直接
/// 拒掉；这边**任何一单在末端窗口里**都能说话。
///
/// 顾客会不会回应由服务端掷骰子决定（大约 95% 的单不会有人开口），所以这里
/// 刻意不承诺「必有回音」——空列表是常态，不是 bug。
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

  void _send() {
    final text = _input.text.trim();
    if (text.isEmpty) return;
    widget.client.sendChatSay(widget.orderId, text);
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
          final lines =
              widget.state.chatLines[widget.orderId] ?? const <ChatLine>[];
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
                          '联系顾客 · ${widget.customerName}',
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
                const Divider(height: 1),
                Expanded(
                  child: lines.isEmpty
                      ? const Center(
                          child: Padding(
                            padding: EdgeInsets.all(24),
                            child: Text(
                              '顾客还没开口。\n说一句试试——不过大多数单子不会有人回。',
                              textAlign: TextAlign.center,
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
                // 预设话术：不想打字时一键发。**文本由服务端补全**，
                // 这里只发枚举名，所以「骑手说了什么」永远可审计。
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

  final ChatLine line;

  @override
  Widget build(BuildContext context) {
    final align = line.isMe ? Alignment.centerRight : Alignment.centerLeft;
    final color = switch (line.from) {
      'rider' => Colors.green.shade100,
      'system' => Colors.blueGrey.shade100,
      // 带赔付的顾客消息标红：这不是情绪，是**钱**。
      'customer' when line.isCostly => Colors.red.shade100,
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
        child: Column(
          crossAxisAlignment:
              line.isMe ? CrossAxisAlignment.end : CrossAxisAlignment.start,
          children: [
            Text('$who：${line.text}'),
            const SizedBox(height: 2),
            Text(
              line.isCostly
                  ? '${line.at} · 赔付 ${yuan(line.compensationCents)} 元'
                  : line.at,
              style: TextStyle(
                fontSize: 11,
                color: line.isCostly ? Colors.red.shade900 : Colors.black54,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

import 'dart:async';

import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';
import '../widgets/order_card.dart';

/// 骑手主界面：大厅 / 我的单 / 战报。
class RiderHomePage extends StatefulWidget {
  const RiderHomePage({super.key, required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  State<RiderHomePage> createState() => _RiderHomePageState();
}

class _RiderHomePageState extends State<RiderHomePage> {
  Timer? _ticker;

  @override
  void initState() {
    super.initState();
    // 每秒重绘一次，让倒计时 / 时钟走字。
    _ticker = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted) setState(() {});
    });
  }

  @override
  void dispose() {
    _ticker?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final state = widget.state;
    return AnimatedBuilder(
      animation: state,
      builder: (context, _) {
        return DefaultTabController(
          length: 3,
          child: Scaffold(
            appBar: AppBar(
              title: _Title(state: state),
              actions: [
                IconButton(
                  tooltip: '重新同步',
                  onPressed: widget.client.resync,
                  icon: const Icon(Icons.sync),
                ),
                IconButton(
                  tooltip: '重连',
                  onPressed: widget.client.reconnect,
                  icon: const Icon(Icons.wifi_protected_setup),
                ),
              ],
              bottom: const TabBar(
                tabs: [
                  Tab(text: '大厅'),
                  Tab(text: '我的单'),
                  Tab(text: '战报'),
                ],
              ),
            ),
            body: Column(
              children: [
                _StatusHeader(state: state, client: widget.client),
                Expanded(
                  child: TabBarView(
                    children: [
                      _HallTab(state: state, client: widget.client),
                      _MyOrdersTab(state: state, client: widget.client),
                      _LedgerTab(state: state),
                    ],
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}

class _Title extends StatelessWidget {
  const _Title({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    final color = switch (state.status) {
      ConnStatus.online => Colors.greenAccent,
      ConnStatus.connecting || ConnStatus.retrying => Colors.orangeAccent,
      _ => Colors.redAccent,
    };
    return Row(
      children: [
        Container(
          width: 10,
          height: 10,
          decoration: BoxDecoration(color: color, shape: BoxShape.circle),
        ),
        const SizedBox(width: 8),
        Text('第 ${state.day} 天 ${clockLabel(state.minuteOfDay)}'),
        const SizedBox(width: 10),
        Text(
          state.weatherLabel,
          style: Theme.of(context).textTheme.bodyMedium,
        ),
      ],
    );
  }
}

/// 顶部骑手状态条。
class _StatusHeader extends StatelessWidget {
  const _StatusHeader({required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    final me = state.me;
    if (me == null) {
      return Container(
        width: double.infinity,
        color: Colors.black12,
        padding: const EdgeInsets.all(12),
        child: const Text('观战中：你看到的是公开信息，无法抢单。'),
      );
    }

    return Container(
      width: double.infinity,
      color: Colors.black12,
      padding: const EdgeInsets.fromLTRB(14, 10, 14, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Text(
                '${me.name} · Lv${me.level}',
                style: Theme.of(context)
                    .textTheme
                    .titleMedium
                    ?.copyWith(fontWeight: FontWeight.bold),
              ),
              const SizedBox(width: 8),
              Text('★ ${me.rating.toStringAsFixed(2)}'),
              const Spacer(),
              Switch(
                value: me.online,
                onChanged: (v) => client.setOnline(v),
              ),
              Text(me.online ? '在线' : '离线'),
            ],
          ),
          const SizedBox(height: 4),
          Row(
            children: [
              _Money(label: '今日到手', cents: me.takehomeCents),
              _Money(label: '时薪', cents: me.hourlyWageCents),
              _Money(label: '钱包', cents: me.cashCents),
              if (me.frozen) const _Badge('冻结', Colors.red),
              if (me.injured) const _Badge('受伤', Colors.orange),
            ],
          ),
          const SizedBox(height: 6),
          Row(
            children: [
              const Text('骑行模式：', style: TextStyle(fontSize: 12)),
              const SizedBox(width: 6),
              ...SpeedMode.values.map(
                (m) => Padding(
                  padding: const EdgeInsets.only(right: 6),
                  child: ChoiceChip(
                    label: Text(m.label, style: const TextStyle(fontSize: 12)),
                    selected: me.speedMode == m,
                    onSelected: (_) => client.setSpeedMode(m),
                  ),
                ),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _Money extends StatelessWidget {
  const _Money({required this.label, required this.cents});

  final String label;
  final int? cents;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(right: 16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(label, style: const TextStyle(fontSize: 11, color: Colors.grey)),
          Text(
            cents == null ? '--' : '¥${yuan(cents!)}',
            style: const TextStyle(fontWeight: FontWeight.w600),
          ),
        ],
      ),
    );
  }
}

class _Badge extends StatelessWidget {
  const _Badge(this.text, this.color);

  final String text;
  final Color color;

  @override
  Widget build(BuildContext context) => Container(
        margin: const EdgeInsets.only(left: 6),
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
        decoration: BoxDecoration(
          color: color.withOpacity(0.15),
          borderRadius: BorderRadius.circular(6),
        ),
        child: Text(text, style: TextStyle(color: color, fontSize: 11)),
      );
}

class _HallTab extends StatelessWidget {
  const _HallTab({required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    if (state.hall.isEmpty) {
      return const _Empty('大厅暂时没单，等等看。');
    }
    return ListView.builder(
      itemCount: state.hall.length,
      itemBuilder: (context, i) => OrderCard(
        order: state.hall[i],
        state: state,
        client: client,
      ),
    );
  }
}

class _MyOrdersTab extends StatelessWidget {
  const _MyOrdersTab({required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    final orders = state.myOrders;
    if (orders.isEmpty) {
      return const _Empty('手上没单。去大厅抢一单吧。');
    }
    return ListView(
      children: [
        for (final o in orders) ...[
          OrderCard(order: o, state: state, client: client, mine: true),
          if (o.task != null)
            Padding(
              padding: const EdgeInsets.only(left: 12, right: 12, bottom: 8),
              child: OutlinedButton.icon(
                onPressed: () => _openTaskSheet(context, o),
                icon: const Icon(Icons.chat_bubble_outline, size: 18),
                label: Text('任务对话：${o.task!.name}'),
              ),
            ),
        ],
      ],
    );
  }

  void _openTaskSheet(BuildContext context, OrderView order) {
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
          final List<TaskLine> lines =
              widget.state.taskLines[widget.orderId] ?? const [];
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
                      ? const _Empty('发一句话开始任务对话，例如「放门口吧」。')
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

class _LedgerTab extends StatelessWidget {
  const _LedgerTab({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    return ListView(
      children: [
        if (state.ledgers.isNotEmpty) ...[
          const _SectionTitle('昨日结算'),
          for (final l in state.ledgers)
            ListTile(
              dense: true,
              title: Text('${l.name}（${l.key}）'),
              subtitle: Text(
                '${l.completed} 单 · 超时 ${l.lateCount} · 在线 ${l.onlineMinutes} 分钟',
              ),
              trailing: Text(
                '¥${yuan(l.takehomeCents)}',
                style: const TextStyle(fontWeight: FontWeight.bold),
              ),
            ),
        ],
        const _SectionTitle('动态'),
        if (state.logs.isEmpty)
          const _Empty('还没有动态。')
        else
          for (final line in state.logs)
            ListTile(
              dense: true,
              leading: Icon(_iconFor(line.kind), size: 18, color: _colorFor(line.kind)),
              title: Text(line.text),
              subtitle: Text(
                clockLabel(line.at.hour * 60 + line.at.minute),
                style: const TextStyle(fontSize: 11),
              ),
            ),
      ],
    );
  }

  IconData _iconFor(LogKind k) => switch (k) {
        LogKind.good => Icons.check_circle_outline,
        LogKind.bad => Icons.error_outline,
        LogKind.money => Icons.paid_outlined,
        LogKind.chat => Icons.chat_bubble_outline,
        LogKind.info => Icons.info_outline,
      };

  Color _colorFor(LogKind k) => switch (k) {
        LogKind.good => Colors.green,
        LogKind.bad => Colors.red,
        LogKind.money => Colors.teal,
        LogKind.chat => Colors.indigo,
        LogKind.info => Colors.grey,
      };
}

class _SectionTitle extends StatelessWidget {
  const _SectionTitle(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.fromLTRB(14, 14, 14, 6),
        child: Text(
          text,
          style: Theme.of(context)
              .textTheme
              .titleSmall
              ?.copyWith(fontWeight: FontWeight.bold),
        ),
      );
}

class _Empty extends StatelessWidget {
  const _Empty(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Center(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Text(text, style: const TextStyle(color: Colors.grey)),
        ),
      );
}

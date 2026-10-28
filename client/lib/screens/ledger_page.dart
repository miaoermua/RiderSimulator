import 'package:flutter/material.dart';

import '../game_state.dart';
import '../protocol.dart';
import 'wallet_page.dart';

/// 账单与战报：今日收入 + 昨日结算。
class LedgerPage extends StatelessWidget {
  const LedgerPage({super.key, required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('账单与战报'),
        actions: [
          IconButton(
            tooltip: '我的钱包',
            icon: const Icon(Icons.account_balance_wallet_outlined),
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => WalletPage(state: state),
              ),
            ),
          ),
        ],
      ),
      body: AnimatedBuilder(
        animation: state,
        builder: (context, _) {
          final me = state.me;
          return ListView(
            children: [
              if (me != null) _TodayCard(me: me, state: state),
              const _SectionTitle('昨日结算'),
              if (state.ledgers.isEmpty)
                const Padding(
                  padding: EdgeInsets.all(24),
                  child: Center(
                    child: Text(
                      '还没有结算记录，送完一天就有了。',
                      style: TextStyle(color: Colors.grey),
                    ),
                  ),
                )
              else
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
          );
        },
      ),
    );
  }
}

class _TodayCard extends StatelessWidget {
  const _TodayCard({required this.me, required this.state});

  final RiderView me;
  final GameState state;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 12, 12, 0),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text('第 ${state.day} 天 · 今日', style: theme.textTheme.labelMedium),
              const SizedBox(height: 6),
              Row(
                crossAxisAlignment: CrossAxisAlignment.end,
                children: [
                  Text(
                    '¥${yuan(me.takehomeCents ?? 0)}',
                    style: theme.textTheme.headlineSmall?.copyWith(
                      fontWeight: FontWeight.bold,
                      color: Colors.green.shade700,
                    ),
                  ),
                  const SizedBox(width: 10),
                  Padding(
                    padding: const EdgeInsets.only(bottom: 4),
                    child: Text(
                      '时薪 ${me.hourlyWageCents == null ? '--' : '¥${yuan(me.hourlyWageCents!)}'}',
                      style: theme.textTheme.bodySmall,
                    ),
                  ),
                ],
              ),
              const Divider(height: 22),
              Row(
                children: [
                  _Cell(label: '已完成', value: '${me.completed} 单'),
                  _Cell(label: '手上/上限', value: '${state.myHeldLabel} 单'),
                  _Cell(label: '罚款', value: '¥${yuan(me.finesCents ?? 0)}'),
                ],
              ),
              const SizedBox(height: 10),
              Row(
                children: [
                  _Cell(label: '等级', value: 'Lv${me.level}'),
                  _Cell(label: '评分', value: me.rating.toStringAsFixed(2)),
                  _Cell(label: '钱包', value: '¥${yuan(me.cashCents ?? 0)}'),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _Cell extends StatelessWidget {
  const _Cell({required this.label, required this.value});

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) => Expanded(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(label, style: const TextStyle(fontSize: 11, color: Colors.grey)),
            Text(value, style: const TextStyle(fontWeight: FontWeight.w600)),
          ],
        ),
      );
}

class _SectionTitle extends StatelessWidget {
  const _SectionTitle(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.fromLTRB(14, 16, 14, 6),
        child: Text(
          text,
          style: Theme.of(context)
              .textTheme
              .titleSmall
              ?.copyWith(fontWeight: FontWeight.bold),
        ),
      );
}

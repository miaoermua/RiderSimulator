import 'package:flutter/material.dart';

import '../game_state.dart';
import '../protocol.dart';

/// 我的钱包：余额 + 每一笔收支。
///
/// 流水是本地累积的（服务端只给日结汇总，没有逐笔明细接口），
/// 所以重连不清空、但换设备看不到历史。
class WalletPage extends StatelessWidget {
  const WalletPage({super.key, required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('我的钱包')),
      body: AnimatedBuilder(
        animation: state,
        builder: (context, _) {
          final me = state.me;
          final entries = state.walletEntries;
          return ListView(
            children: [
              if (me != null) _BalanceCard(me: me),
              const Padding(
                padding: EdgeInsets.fromLTRB(16, 16, 16, 4),
                child: Text(
                  '收支明细',
                  style: TextStyle(fontWeight: FontWeight.bold),
                ),
              ),
              if (entries.isEmpty)
                const Padding(
                  padding: EdgeInsets.all(32),
                  child: Center(
                    child: Text(
                      '还没有收支记录，跑完第一单就有了。',
                      style: TextStyle(color: Colors.grey),
                    ),
                  ),
                )
              else
                for (final e in entries)
                  ListTile(
                    dense: true,
                    leading: _icon(e),
                    title: Text(e.title, maxLines: 1, overflow: TextOverflow.ellipsis),
                    subtitle: Text(_stamp(e.at)),
                    trailing: Text(
                      '${e.income ? '+' : '-'}¥${yuan(e.cents.abs())}',
                      style: TextStyle(
                        fontWeight: FontWeight.bold,
                        color: e.income ? Colors.green.shade700 : Colors.red.shade600,
                      ),
                    ),
                  ),
              const Padding(
                padding: EdgeInsets.fromLTRB(16, 12, 16, 24),
                child: Text(
                  '明细在本地累积，重连不会清空；每天的汇总在「账单与战报」里。',
                  style: TextStyle(fontSize: 11, color: Colors.grey),
                ),
              ),
            ],
          );
        },
      ),
    );
  }

  static Icon _icon(WalletEntry e) {
    if (e.kind == 'fine' || !e.income) {
      return Icon(Icons.remove_circle_outline, color: Colors.red.shade300);
    }
    if (e.kind == 'activity') {
      return Icon(Icons.emoji_events_outlined, color: Colors.orange.shade400);
    }
    return Icon(Icons.add_circle_outline, color: Colors.green.shade400);
  }

  static String _stamp(DateTime t) {
    String two(int v) => v.toString().padLeft(2, '0');
    return '${two(t.month)}-${two(t.day)} ${two(t.hour)}:${two(t.minute)}';
  }
}

class _BalanceCard extends StatelessWidget {
  const _BalanceCard({required this.me});

  final RiderView me;

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
              Text('钱包余额', style: theme.textTheme.labelSmall),
              Text(
                '¥${yuan(me.cashCents ?? 0)}',
                style: theme.textTheme.headlineMedium?.copyWith(
                  fontWeight: FontWeight.bold,
                  color: Colors.green.shade700,
                ),
              ),
              const SizedBox(height: 12),
              Row(
                children: [
                  _Money(label: '今日到手', cents: me.takehomeCents),
                  _Money(label: '时薪', cents: me.hourlyWageCents),
                  _Money(label: '罚款', cents: me.finesCents),
                ],
              ),
            ],
          ),
        ),
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
    return Expanded(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(label, style: const TextStyle(fontSize: 11, color: Colors.grey)),
          Text(
            '¥${yuan(cents ?? 0)}',
            style: const TextStyle(fontWeight: FontWeight.w600),
          ),
        ],
      ),
    );
  }
}

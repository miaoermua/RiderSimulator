import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';
import '../screens/ledger_page.dart';
import '../screens/notifications_page.dart';
import '../screens/running_settings_sheet.dart';

/// 折叠菜单：骑手信息、钱包金额、已完成单数，以及各页入口。
class RiderDrawer extends StatelessWidget {
  const RiderDrawer({super.key, required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    return Drawer(
      child: AnimatedBuilder(
        animation: state,
        builder: (context, _) {
          final me = state.me;
          return ListView(
            padding: EdgeInsets.zero,
            children: [
              _Header(state: state),
              if (me != null) ...[
                _Wallet(me: me),
                _Counters(me: me),
              ],
              const Divider(height: 1),
              ListTile(
                leading: const Icon(Icons.notifications_none),
                title: const Text('通知消息'),
                trailing: state.unreadLogs == 0
                    ? null
                    : Badge(label: Text('${state.unreadLogs}')),
                onTap: () {
                  Navigator.pop(context);
                  Navigator.of(context).push<void>(
                    MaterialPageRoute<void>(
                      builder: (_) => NotificationsPage(state: state),
                    ),
                  );
                },
              ),
              ListTile(
                leading: const Icon(Icons.receipt_long_outlined),
                title: const Text('账单与战报'),
                subtitle: Text('第 ${state.day} 天 · 昨日结算'),
                onTap: () {
                  Navigator.pop(context);
                  Navigator.of(context).push<void>(
                    MaterialPageRoute<void>(
                      builder: (_) => LedgerPage(state: state),
                    ),
                  );
                },
              ),
              ListTile(
                leading: const Icon(Icons.tune),
                title: const Text('跑单设置'),
                onTap: () {
                  Navigator.pop(context);
                  showRunningSettings(context, state: state, client: client);
                },
              ),
              const Divider(height: 1),
              ListTile(
                leading: const Icon(Icons.sync),
                title: const Text('重新同步'),
                onTap: () {
                  Navigator.pop(context);
                  client.resync();
                },
              ),
              ListTile(
                leading: const Icon(Icons.wifi_protected_setup),
                title: const Text('重连'),
                onTap: () {
                  Navigator.pop(context);
                  client.reconnect();
                },
              ),
              const SizedBox(height: 12),
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 16),
                child: Text(
                  state.serverUrl,
                  style: const TextStyle(fontSize: 11, color: Colors.grey),
                ),
              ),
            ],
          );
        },
      ),
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({required this.state});

  final GameState state;

  @override
  Widget build(BuildContext context) {
    final me = state.me;
    final name = me?.name ?? '观战者';
    return UserAccountsDrawerHeader(
      margin: EdgeInsets.zero,
      currentAccountPicture: CircleAvatar(
        backgroundColor: Colors.white,
        child: Text(
          name.isEmpty ? '?' : name.substring(0, 1),
          style: const TextStyle(fontSize: 22, fontWeight: FontWeight.bold),
        ),
      ),
      accountName: Row(
        children: [
          Flexible(
            child: Text(
              name,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(fontWeight: FontWeight.bold),
            ),
          ),
          if (me != null) ...[
            const SizedBox(width: 6),
            Text('Lv${me.level}', style: const TextStyle(fontSize: 12)),
          ],
        ],
      ),
      accountEmail: me == null
          ? Text('未登录骑手身份')
          : Row(
              children: [
                Icon(
                  Icons.circle,
                  size: 8,
                  color: me.online ? Colors.greenAccent : Colors.grey,
                ),
                const SizedBox(width: 4),
                Text(me.online ? '在线' : '离线'),
                const SizedBox(width: 10),
                Text('★ ${me.rating.toStringAsFixed(2)}'),
                const SizedBox(width: 10),
                Text(me.speedMode.label),
              ],
            ),
      otherAccountsPictures: [
        Container(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
          decoration: BoxDecoration(
            color: Colors.white24,
            borderRadius: BorderRadius.circular(10),
          ),
          child: Center(
            child: Text(
              '第 ${state.day} 天 ${clockLabel(state.minuteOfDay)}',
              style: const TextStyle(fontSize: 11, color: Colors.white),
            ),
          ),
        ),
      ],
    );
  }
}

class _Wallet extends StatelessWidget {
  const _Wallet({required this.me});

  final RiderView me;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 12, 12, 4),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(14),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '钱包余额',
                style: Theme.of(context).textTheme.labelSmall,
              ),
              Text(
                '¥${yuan(me.cashCents ?? 0)}',
                style: Theme.of(context).textTheme.headlineSmall?.copyWith(
                      fontWeight: FontWeight.bold,
                      color: Colors.green.shade700,
                    ),
              ),
              const SizedBox(height: 8),
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

class _Counters extends StatelessWidget {
  const _Counters({required this.me});

  final RiderView me;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 0, 12, 10),
      child: Row(
        children: [
          _Metric(
            icon: Icons.check_circle_outline,
            label: '已完成',
            value: '${me.completed}',
            unit: '单',
          ),
          _Metric(
            icon: Icons.shopping_bag_outlined,
            label: '手上',
            value: '${me.held.length}',
            unit: '单',
          ),
        ],
      ),
    );
  }
}

class _Metric extends StatelessWidget {
  const _Metric({
    required this.icon,
    required this.label,
    required this.value,
    required this.unit,
  });

  final IconData icon;
  final String label;
  final String value;
  final String unit;

  @override
  Widget build(BuildContext context) {
    return Expanded(
      child: Column(
        children: [
          Icon(icon, size: 18, color: Colors.grey.shade700),
          const SizedBox(height: 2),
          RichText(
            text: TextSpan(
              style: DefaultTextStyle.of(context).style,
              children: [
                TextSpan(
                  text: value,
                  style: const TextStyle(
                    fontSize: 18,
                    fontWeight: FontWeight.bold,
                  ),
                ),
                TextSpan(text: ' $unit', style: const TextStyle(fontSize: 11)),
              ],
            ),
          ),
          Text(label, style: const TextStyle(fontSize: 11, color: Colors.grey)),
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
    return Expanded(
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

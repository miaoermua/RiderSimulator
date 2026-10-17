import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';

/// 设置（骑手端）。
///
/// 从抽屉与跑单设置里收拢过来的四样东西：
/// **重新同步 / 重连 / 骑行模式 / 派单规则**。
/// 出勤、定位上报、地图底图仍留在「跑单设置」——那是跑单时随手要改的。
class SettingsPage extends StatelessWidget {
  const SettingsPage({super.key, required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('设置')),
      body: AnimatedBuilder(
        animation: state,
        builder: (context, _) {
          final me = state.me;
          final cfg = state.config;
          return ListView(
            padding: const EdgeInsets.fromLTRB(16, 12, 16, 24),
            children: [
              const _Label('连接'),
              _Rule('服务器', state.serverUrl),
              _Rule('状态', _statusLabel(state.status)),
              const SizedBox(height: 8),
              Wrap(
                spacing: 8,
                children: [
                  OutlinedButton.icon(
                    onPressed: client.resync,
                    icon: const Icon(Icons.sync, size: 18),
                    label: const Text('重新同步'),
                  ),
                  OutlinedButton.icon(
                    onPressed: client.reconnect,
                    icon: const Icon(Icons.wifi_protected_setup, size: 18),
                    label: const Text('重连'),
                  ),
                ],
              ),
              const Divider(height: 28),
              const _Label('骑行模式'),
              const SizedBox(height: 6),
              if (me == null)
                const Text('观战模式：没有骑手身份，不能选骑行模式。',
                    style: TextStyle(fontSize: 12, color: Colors.grey))
              else ...[
                Wrap(
                  spacing: 8,
                  children: [
                    for (final m in SpeedMode.values)
                      ChoiceChip(
                        label: Text(m.label),
                        selected: me.speedMode == m,
                        onSelected: (_) => client.setSpeedMode(m),
                      ),
                  ],
                ),
                const SizedBox(height: 6),
                const Text(
                  '越快越省时间，但被抓/受伤的概率也越高。',
                  style: TextStyle(fontSize: 12, color: Colors.grey),
                ),
              ],
              const Divider(height: 28),
              const _Label('当前派单规则（控制端设定）'),
              const SizedBox(height: 4),
              _Rule('抢单窗口', '${cfg.claimWindowSecs} 秒'),
              _Rule('背单',
                  cfg.backorderEnabled ? '开启 · 最多 ${cfg.backorderMax} 单' : '关闭'),
              _Rule('每日单量上限', '${cfg.maxOrdersPerDay} 单'),
              _Rule('平台抽水', '${(cfg.commissionRate * 100).toStringAsFixed(1)}%'),
              _Rule('时段',
                  cfg.isPeak ? '高峰 · ×${cfg.surgeMultiplier.toStringAsFixed(2)}' : '平峰'),
              _Rule('世界速度', '×${cfg.speedMultiplier}${cfg.paused ? '（已暂停）' : ''}'),
              _Rule('取送半径', '${cfg.arriveRadiusM} 米'),
              const Divider(height: 28),
              const _Label('关于'),
              const _Rule('版本', '0.2.3'),
              const _Rule('客户端', '骑手端 · arm64-v8a'),
            ],
          );
        },
      ),
    );
  }

  static String _statusLabel(ConnStatus s) => switch (s) {
        ConnStatus.idle => '未连接',
        ConnStatus.connecting => '连接中',
        ConnStatus.online => '在线',
        ConnStatus.retrying => '重连中',
        ConnStatus.closed => '已断开',
      };
}

class _Label extends StatelessWidget {
  const _Label(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    return Text(
      text,
      style: Theme.of(context)
          .textTheme
          .titleSmall
          ?.copyWith(fontWeight: FontWeight.bold),
    );
  }
}

class _Rule extends StatelessWidget {
  const _Rule(this.label, this.value);

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 3),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 92,
            child: Text(label,
                style: const TextStyle(fontSize: 12, color: Colors.grey)),
          ),
          Expanded(
            child: Text(value, style: const TextStyle(fontSize: 13)),
          ),
        ],
      ),
    );
  }
}

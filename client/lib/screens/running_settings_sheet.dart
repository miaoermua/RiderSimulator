import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../protocol.dart';

/// 跑单设置。
///
/// 只放**骑手自己能改的**开关（出勤、骑行模式）；派单规则由控制端
/// 「烽火台」设定，这里只做展示——设置项不能是假开关。
Future<void> showRunningSettings(
  BuildContext context, {
  required GameState state,
  required GameClient client,
}) {
  return showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    showDragHandle: true,
    builder: (_) => _RunningSettingsSheet(state: state, client: client),
  );
}

class _RunningSettingsSheet extends StatelessWidget {
  const _RunningSettingsSheet({required this.state, required this.client});

  final GameState state;
  final GameClient client;

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: state,
      builder: (context, _) {
        final me = state.me;
        final cfg = state.config;
        return SafeArea(
          child: ConstrainedBox(
            constraints: BoxConstraints(
              maxHeight: MediaQuery.of(context).size.height * 0.75,
            ),
            child: ListView(
              shrinkWrap: true,
              padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
              children: [
                Text(
                  '跑单设置',
                  style: Theme.of(context)
                      .textTheme
                      .titleMedium
                      ?.copyWith(fontWeight: FontWeight.bold),
                ),
                const SizedBox(height: 8),
                if (me == null)
                  const Padding(
                    padding: EdgeInsets.symmetric(vertical: 12),
                    child: Text('观战模式：没有骑手身份，只能看。'),
                  )
                else ...[
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    title: const Text('出勤'),
                    subtitle: Text(me.online ? '在线，平台会给你派单' : '离线，不接单也不派单'),
                    value: me.online,
                    onChanged: (v) => client.setOnline(v),
                  ),
                  const Divider(height: 1),
                  const SizedBox(height: 10),
                  const _Label('骑行模式'),
                  const SizedBox(height: 6),
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
                  const Divider(height: 26),
                ],
                const _Label('当前派单规则（控制端设定）'),
                const SizedBox(height: 6),
                _Rule('抢单窗口', '${cfg.claimWindowSecs} 秒'),
                _Rule('背单', cfg.backorderEnabled ? '开启 · 最多 ${cfg.backorderMax} 单' : '关闭'),
                _Rule('每日单量上限', '${cfg.maxOrdersPerDay} 单'),
                _Rule('平台抽水', '${(cfg.commissionRate * 100).toStringAsFixed(1)}%'),
                _Rule('时段', cfg.isPeak ? '高峰 · ×${cfg.surgeMultiplier.toStringAsFixed(2)}' : '平峰'),
                _Rule('世界速度', '×${cfg.speedMultiplier}${cfg.paused ? '（已暂停）' : ''}'),
                const Divider(height: 26),
                const _Label('连接'),
                const SizedBox(height: 4),
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
              ],
            ),
          ),
        );
      },
    );
  }

  static String _statusLabel(ConnStatus s) => switch (s) {
        ConnStatus.idle => '未连接',
        ConnStatus.connecting => '连接中',
        ConnStatus.online => '已连接',
        ConnStatus.retrying => '重连中',
        ConnStatus.closed => '已断开',
      };
}

class _Label extends StatelessWidget {
  const _Label(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Text(
        text,
        style: Theme.of(context)
            .textTheme
            .titleSmall
            ?.copyWith(fontWeight: FontWeight.bold),
      );
}

/// 「名称 —— 值」一行。
class _Rule extends StatelessWidget {
  const _Rule(this.name, this.value);

  final String name;
  final String value;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.symmetric(vertical: 3),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            SizedBox(
              width: 110,
              child: Text(
                name,
                style: const TextStyle(fontSize: 13, color: Colors.grey),
              ),
            ),
            Expanded(
              child: Text(value, style: const TextStyle(fontSize: 13)),
            ),
          ],
        ),
      );
}

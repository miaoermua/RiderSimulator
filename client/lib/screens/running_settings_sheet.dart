import 'package:flutter/material.dart';

import '../game_client.dart';
import '../game_state.dart';
import '../location_service.dart';
import '../map/basemaps.dart';

/// 跑单设置。
///
/// 只放跑单当下要随手改的：出勤、定位上报、地图底图。
/// 连接、骑行模式、派单规则已经收进「设置」页。
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
      animation: Listenable.merge([state, LocationService.instance]),
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
                ],
                const _Label('定位上报'),
                const SizedBox(height: 6),
                Wrap(
                  spacing: 8,
                  children: [
                    for (final r in LocationRate.values)
                      ChoiceChip(
                        label: Text(r.label),
                        selected: LocationService.instance.rate == r,
                        onSelected: (_) => LocationService.instance.setRate(r),
                      ),
                  ],
                ),
                const SizedBox(height: 6),
                Text(
                  '${LocationService.instance.rate.detail}。越勤越准、也越耗电；只在前台跑单时定位，关了就不问权限也不上报。',
                  style: const TextStyle(fontSize: 12, color: Colors.grey),
                ),
                const SizedBox(height: 4),
                Text(
                  _locationStatus(),
                  style: TextStyle(
                    fontSize: 12,
                    color: LocationService.instance.error != null
                        ? Colors.orange.shade800
                        : Colors.grey,
                  ),
                ),
                const Divider(height: 26),
                const _Label('地图底图'),
                const SizedBox(height: 6),
                Wrap(
                  spacing: 8,
                  children: [
                    ChoiceChip(
                      label: const Text('跟随坐标系'),
                      selected: state.basemapOverride == null,
                      onSelected: (_) => state.setBasemapOverride(null),
                    ),
                    for (final bm in basemaps.values)
                      ChoiceChip(
                        label: Text(bm.label),
                        selected: state.basemapOverride == bm.id,
                        onSelected: (_) => state.setBasemapOverride(bm.id),
                      ),
                  ],
                ),
                const SizedBox(height: 6),
                Text(
                  state.basemapMismatch
                      ? '下发的是 ${cfg.coordSystem.label}，当前底图与它不一致，点位会偏几百米。'
                      : '底图与下发坐标（${cfg.coordSystem.label}）是对齐的。',
                  style: TextStyle(
                    fontSize: 12,
                    color: state.basemapMismatch
                        ? Colors.orange.shade800
                        : Colors.grey,
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  static String _locationStatus() {
    final loc = LocationService.instance;
    if (loc.error != null) return loc.error!;
    if (!loc.running) {
      return loc.rate == LocationRate.off ? '已关闭：骑手停在服务端最后给的位置。' : '尚未启动';
    }
    final at = loc.lastFixAt;
    if (at == null) return '已申请权限，等待第一次定位…';
    final t = '${at.hour.toString().padLeft(2, '0')}:'
        '${at.minute.toString().padLeft(2, '0')}:'
        '${at.second.toString().padLeft(2, '0')}';
    return '运行中 · 最近 $t · '
        '世界坐标 ${loc.lastLat!.toStringAsFixed(5)}, ${loc.lastLng!.toStringAsFixed(5)}';
  }
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

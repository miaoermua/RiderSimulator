import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:permission_handler/permission_handler.dart';

import '../alert_center.dart';
import '../alert_event.dart';
import '../game_client.dart';
import '../game_state.dart';
import '../navigation_service.dart';
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
        // 提示音开关存在 AlertCenter 里，跟游戏状态不是同一个源。
        animation: Listenable.merge([state, AlertCenter.instance]),
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
              const _Label('提醒'),
              SwitchListTile(
                contentPadding: EdgeInsets.zero,
                dense: true,
                value: AlertCenter.instance.soundEnabled,
                title: const Text('提示音', style: TextStyle(fontSize: 13)),
                subtitle: const Text(
                  '默认用系统自带的通知铃声；也可以给每类提醒换成手机里的音频。',
                  style: TextStyle(fontSize: 12, color: Colors.grey),
                ),
                onChanged: AlertCenter.instance.setSoundEnabled,
              ),
              if (AlertCenter.canPickCustomSound)
                for (final slot in AlertSoundSlot.values)
                  _SoundTile(slot: slot),
              const _PermissionBlock(),
              const Divider(height: 28),
              const _Label('界面'),
              SwitchListTile(
                contentPadding: EdgeInsets.zero,
                dense: true,
                value: state.platformPref,
                title: const Text('平台偏好', style: TextStyle(fontSize: 13)),
                subtitle: const Text(
                  '换一套首页版式：路线规划挪到「待送」右边，点开是页内地图。'
                  '只影响你自己的界面，不改变派单范围。',
                  style: TextStyle(fontSize: 12, color: Colors.grey),
                ),
                onChanged: state.setPlatformPref,
              ),
              // 只有「系统默认」一个选项时（Web / 桌面）不给点——
              // 点开一个只有一项的选择器没有意义。
              Builder(
                builder: (context) {
                  final apps = NavApp.forPlatform();
                  return ListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('默认导航应用', style: TextStyle(fontSize: 13)),
                    subtitle: const Text(
                      '点订单上的导航图标时交给哪个地图',
                      style: TextStyle(fontSize: 12, color: Colors.grey),
                    ),
                    trailing: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Text(
                          state.navApp.label,
                          style: const TextStyle(fontSize: 13),
                        ),
                        const Icon(Icons.chevron_right, size: 18),
                      ],
                    ),
                    onTap: apps.length <= 1
                        ? null
                        : () async {
                            final picked = await showModalBottomSheet<NavApp>(
                              context: context,
                              builder: (sheetCtx) => SafeArea(
                                child: Column(
                                  mainAxisSize: MainAxisSize.min,
                                  children: [
                                    const Padding(
                                      padding:
                                          EdgeInsets.fromLTRB(16, 14, 16, 6),
                                      child: Text(
                                        '默认导航应用',
                                        style: TextStyle(
                                          fontSize: 13,
                                          fontWeight: FontWeight.w600,
                                        ),
                                      ),
                                    ),
                                    for (final item in apps)
                                      ListTile(
                                        title: Text(item.label),
                                        trailing: item == state.navApp
                                            ? const Icon(Icons.check, size: 20)
                                            : null,
                                        onTap: () =>
                                            Navigator.of(sheetCtx).pop(item),
                                      ),
                                  ],
                                ),
                              ),
                            );
                            if (picked != null) {
                              await state.setNavApp(picked);
                            }
                          },
                  );
                },
              ),
              const Divider(height: 28),
              const _Label('界面'),
              SwitchListTile(
                contentPadding: EdgeInsets.zero,
                dense: true,
                value: state.platformPref,
                title: const Text('平台偏好', style: TextStyle(fontSize: 13)),
                subtitle: const Text(
                  '换一套首页版式：路线规划挪到「待送」右边，点开是页内地图。'
                  '只影响你自己的界面，不改变派单范围。',
                  style: TextStyle(fontSize: 12, color: Colors.grey),
                ),
                onChanged: state.setPlatformPref,
              ),
              // 只有「系统默认」一个选项时（Web / 桌面）不给点——
              // 点开一个只有一项的选择器没有意义。
              Builder(
                builder: (context) {
                  final apps = NavApp.forPlatform();
                  return ListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    title: const Text('默认导航应用', style: TextStyle(fontSize: 13)),
                    subtitle: const Text(
                      '点订单上的导航图标时交给哪个地图',
                      style: TextStyle(fontSize: 12, color: Colors.grey),
                    ),
                    trailing: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Text(
                          state.navApp.label,
                          style: const TextStyle(fontSize: 13),
                        ),
                        const Icon(Icons.chevron_right, size: 18),
                      ],
                    ),
                    onTap: apps.length <= 1
                        ? null
                        : () async {
                            final picked = await showModalBottomSheet<NavApp>(
                              context: context,
                              builder: (sheetCtx) => SafeArea(
                                child: Column(
                                  mainAxisSize: MainAxisSize.min,
                                  children: [
                                    const Padding(
                                      padding:
                                          EdgeInsets.fromLTRB(16, 14, 16, 6),
                                      child: Text(
                                        '默认导航应用',
                                        style: TextStyle(
                                          fontSize: 13,
                                          fontWeight: FontWeight.w600,
                                        ),
                                      ),
                                    ),
                                    for (final item in apps)
                                      ListTile(
                                        title: Text(item.label),
                                        trailing: item == state.navApp
                                            ? const Icon(Icons.check, size: 20)
                                            : null,
                                        onTap: () =>
                                            Navigator.of(sheetCtx).pop(item),
                                      ),
                                  ],
                                ),
                              ),
                            );
                            if (picked != null) {
                              await state.setNavApp(picked);
                            }
                          },
                  );
                },
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
              _Rule(
                  '背单',
                  cfg.backorderEnabled
                      ? '开启 · 最多 ${cfg.backorderMax} 单'
                      : '关闭'),
              _Rule('每日单量上限', '${cfg.maxOrdersPerDay} 单'),
              _Rule(
                  '平台抽水', '${(cfg.commissionRate * 100).toStringAsFixed(1)}%'),
              _Rule(
                  '时段',
                  cfg.isPeak
                      ? '高峰 · ×${cfg.surgeMultiplier.toStringAsFixed(2)}'
                      : '平峰'),
              _Rule('世界速度',
                  '×${cfg.speedMultiplier}${cfg.paused ? '（已暂停）' : ''}'),
              _Rule('取送半径', '${cfg.arriveRadiusM} 米'),
              const Divider(height: 28),
              const _Label('关于'),
              const _Rule('版本', '0.2.6'),
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

/// 系统权限状态：能看、能跳过去开，**不在这里申请**。
///
/// 为什么需要这一块：Android / iOS 的权限弹窗**只会出现一两次**。
/// 用户点过「拒绝」之后（iOS 一次、Android 两次），系统就永久静默，
/// 代码再调 `requestPermission()` 也不会弹——只能由用户自己去系统设置里改。
///
/// 申请逻辑仍然散在原来的地方（启动时的 `AlertCenter.init`、进主界面的
/// `LocationService.attach`），因为那里各自知道上下文。这里只补上缺的那半截：
/// **拒绝之后去哪儿改**。没有这一块，用户看到的现象就是「没反应」。
class _PermissionBlock extends StatefulWidget {
  const _PermissionBlock();

  @override
  State<_PermissionBlock> createState() => _PermissionBlockState();
}

class _PermissionBlockState extends State<_PermissionBlock>
    with WidgetsBindingObserver {
  /// null = 还没查出来（首帧）。Web 上一直保持 null，整块不渲染。
  PermissionStatus? _location;
  PermissionStatus? _notification;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _refresh();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  /// 从系统设置切回来时重查一次——否则用户开完权限回来，
  /// 这一块还停在「未允许」，看着像没生效。
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) _refresh();
  }

  Future<void> _refresh() async {
    // 浏览器没有这两项权限概念，`Permission.*.status` 在 Web 上也不可用。
    if (kIsWeb) return;
    try {
      final loc = await Permission.location.status;
      final notif = await Permission.notification.status;
      if (!mounted) return;
      setState(() {
        _location = loc;
        _notification = notif;
      });
    } catch (e) {
      debugPrint('settings: 读权限状态失败 $e');
    }
  }

  /// 真正需要用户动手的状态。`limited` / `provisional` 已经够用，不算缺。
  static bool _needsAction(PermissionStatus? s) =>
      s != null && (s.isDenied || s.isPermanentlyDenied || s.isRestricted);

  static String _text(PermissionStatus? s) {
    if (s == null) return '—';
    if (s.isGranted) return '已允许';
    if (s.isLimited) return '仅允许部分';
    if (s.isProvisional) return '临时允许';
    if (s.isPermanentlyDenied) return '已拒绝 · 系统不再询问';
    if (s.isRestricted) return '受系统限制';
    return '未允许';
  }

  @override
  Widget build(BuildContext context) {
    if (kIsWeb || (_location == null && _notification == null)) {
      return const SizedBox.shrink();
    }
    final need = _needsAction(_location) || _needsAction(_notification);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Divider(height: 28),
        const _Label('系统权限'),
        _Rule('位置', _text(_location)),
        _Rule('通知', _text(_notification)),
        if (need) ...[
          const SizedBox(height: 6),
          const Text(
            '提示过的权限如果被拒绝，系统不会再弹窗，需要在这里去设置里打开。',
            style: TextStyle(fontSize: 12, color: Colors.grey),
          ),
          const SizedBox(height: 8),
          OutlinedButton.icon(
            onPressed: () async {
              await openAppSettings();
              // 回来时 didChangeAppLifecycleState 会再查一次；
              // 这里也查一次，防止某些机型不触发 resumed。
              await _refresh();
            },
            icon: const Icon(Icons.settings_outlined, size: 18),
            label: const Text('去系统设置'),
          ),
        ],
      ],
    );
  }
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

/// 一类提醒的音效：显示当前音源，可试听 / 换文件 / 恢复系统默认。
class _SoundTile extends StatelessWidget {
  const _SoundTile({required this.slot});

  final AlertSoundSlot slot;

  static String _title(AlertSoundSlot slot) {
    switch (slot) {
      case AlertSoundSlot.assign:
        return '指派单';
      case AlertSoundSlot.online:
        return '上线';
      case AlertSoundSlot.offline:
        return '下线';
    }
  }

  @override
  Widget build(BuildContext context) {
    final center = AlertCenter.instance;
    final custom = center.customPath(slot) != null;
    return ListTile(
      contentPadding: EdgeInsets.zero,
      dense: true,
      visualDensity: VisualDensity.compact,
      title: Text(_title(slot), style: const TextStyle(fontSize: 13)),
      subtitle: Text(
        center.soundLabel(slot),
        style: const TextStyle(fontSize: 12, color: Colors.grey),
        overflow: TextOverflow.ellipsis,
      ),
      // 点整行就是试听：改完不用再去找试听按钮。
      onTap: () => center.preview(slot),
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          IconButton(
            tooltip: '试听',
            visualDensity: VisualDensity.compact,
            icon: const Icon(Icons.play_arrow, size: 20),
            onPressed: () => center.preview(slot),
          ),
          IconButton(
            tooltip: '选择音频',
            visualDensity: VisualDensity.compact,
            icon: const Icon(Icons.folder_open, size: 20),
            onPressed: () => center.pickSound(slot),
          ),
          if (custom)
            IconButton(
              tooltip: '恢复系统默认',
              visualDensity: VisualDensity.compact,
              icon: const Icon(Icons.restart_alt, size: 20),
              onPressed: () => center.resetSound(slot),
            ),
        ],
      ),
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

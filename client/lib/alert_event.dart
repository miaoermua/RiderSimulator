/// 需要惊动骑手的事件。
///
/// 只描述「发生了什么」，不管怎么弹、放什么音——那是 [AlertCenter] 的事。
/// 单独成文件是为了让 `game_state.dart` 保持可单测：它 import 的这里
/// 不依赖任何原生插件，测试时不会把通知插件一起拖进来。
enum AlertKind {
  /// 平台派来的单，可以拒（拒了吃额度）。
  assignOrder,

  /// 强制派单，只能同意。
  forcedAssign,

  /// 新活动上架。
  activity,

  /// 自己点了上线。
  online,

  /// 自己点了下线。
  offline,

  /// 抢单成功。
  ///
  /// 只出声、不落通知栏：屏幕亮着、手刚离开滑块，再弹一条系统通知是噪音。
  /// 界面里的成功态（[OrderCard] 抢单区）已经把事情说清楚了。
  claimSuccess,
}

/// 设置里能单独配音效的槽位。
///
/// 只列**用户能自定义**的三个：上架活动这类系统提示音够用，不占设置项。
enum AlertSoundSlot {
  /// 指派单 / 强制派单。
  assign,

  /// 出勤上线。
  online,

  /// 出勤下线。
  offline,
}

class RiderAlert {
  const RiderAlert({required this.kind, this.title = '', this.body = ''});

  final AlertKind kind;

  /// 通知标题，也是锁屏上第一眼看到的那行。
  final String title;

  /// 一句话说清是什么单 / 什么活动。
  final String body;

  /// 要不要落到通知栏。
  ///
  /// 上下线是自己刚按的按钮，屏幕还亮着，再弹一条通知纯属打扰，
  /// 只出声就够了。
  bool get showsNotification =>
      kind != AlertKind.online &&
      kind != AlertKind.offline &&
      kind != AlertKind.claimSuccess;

  /// 对应的音效槽。`null` = 用系统默认通知音，不进设置。
  AlertSoundSlot? get soundSlot {
    switch (kind) {
      case AlertKind.assignOrder:
      case AlertKind.forcedAssign:
      // 抢单成功复用「来单」那一套音效：用户已经能换它，不必多一个设置项。
      case AlertKind.claimSuccess:
        return AlertSoundSlot.assign;
      case AlertKind.online:
        return AlertSoundSlot.online;
      case AlertKind.offline:
        return AlertSoundSlot.offline;
      case AlertKind.activity:
        return null;
    }
  }
}

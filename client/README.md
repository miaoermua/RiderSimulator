# 骑手端（Flutter）

骑手模拟器的真人骑手客户端。与服务端通过 WebSocket 通信，协议见
[`../packages/protocol.json`](../packages/protocol.json)。

## 跑起来

```bash
# 1. 先起服务端
cd ../server && RIDER_SIM_ALLOW_CONTROL=true cargo run

# 2. 再起客户端
flutter pub get
flutter run            # 桌面 / 模拟器 / Chrome 均可
```

默认连 `ws://127.0.0.1:8080/ws`。在登录页改地址；令牌填：

| 令牌 | 效果 |
|---|---|
| `rider:1` | 绑定到 1 号骑手（内置模拟骑手，可直接跑） |
| `new:张三` | 现场建一个真人骑手 |
| 留空 | 观战 |

## 目录

```
lib/
  protocol.dart      # 协议常量 + 数据模型（与 protocol.json 一一对应）
  game_state.dart    # 客户端世界状态：吃事件、发通知、驱动 UI
  game_client.dart   # WebSocket 连接、握手、自动重连、上行指令
  screens/
    login_page.dart  # 填地址与令牌
    rider_home.dart  # 主界面：大厅 / 我的单 / 战报
  widgets/
    order_card.dart  # 订单卡片（含抢单倒计时）
```

## 还没做的

这是**骨架**，联调通了「登录 → 看大厅 → 抢单 → 到店/送达 → 任务对话 → 看战报」这条主线：

- 地图：目前只有列表，没有高德图层与轨迹回放。
- 抢单窗口倒计时用的是服务端下发的时间戳插值，未做本地校时。
- 离线缓存、推送、埋点都没做。

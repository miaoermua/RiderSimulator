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

## 打包标识（包名 / Bundle ID）

已定：**`net.miaoer.game.crowdsource`** —— Android 的 `applicationId` 与 iOS 的
`PRODUCT_BUNDLE_IDENTIFIER` 用同一个值。

`applicationId` 一旦上架就**永久不可改**（改名等于换一个应用，安装量与评论都不跟随），
所以这个值先钉在这里，别在 `flutter create` 的时候随手生成一个。

`client/android/` 目前还没生成（仓库里只有 `lib/`）。装了 Flutter 之后这样生成、并把包名改到位：

```bash
cd client
# --org 只是前缀，模板给出的会是 net.miaoer.game.rider_client，包名按下面手动改
flutter create --org net.miaoer.game --project-name rider_client \
  --platforms=android,ios,macos,web .
```

`--project-name` 保持 `rider_client`，Dart 包名不变；`macos` / `web` 是给本地调试用的
（`flutter run` 要有平台目录），打包只关心 `android` / `ios`。

生成后改这四处：

| 文件 | 改什么 |
|---|---|
| `android/app/build.gradle.kts`（旧版模板是 `.gradle`） | `namespace` 和 `defaultConfig.applicationId` 都设成 `net.miaoer.game.crowdsource` |
| `android/app/src/main/kotlin/…/MainActivity.kt` | 挪到 `kotlin/net/miaoer/game/crowdsource/`，首行改成 `package net.miaoer.game.crowdsource` |
| `ios/Runner.xcodeproj/project.pbxproj` | `PRODUCT_BUNDLE_IDENTIFIER` 两处：`net.miaoer.game.crowdsource` 与 `net.miaoer.game.crowdsource.RunnerTests` |
| `android/app/src/main/AndroidManifest.xml` | `android:label` 是**应用显示名**（中文不受包名影响），跟包名两回事 |

`namespace` 必须和 `MainActivity.kt` 的 `package` 一致，否则清单里的 `.MainActivity` 解析不到。

## 还没做的

这是**骨架**，联调通了「登录 → 看大厅 → 抢单 → 到店/送达 → 任务对话 → 看战报」这条主线：

- 地图：目前只有列表，没有高德图层与轨迹回放。
- 抢单窗口倒计时用的是服务端下发的时间戳插值，未做本地校时。
- 离线缓存、推送、埋点都没做。

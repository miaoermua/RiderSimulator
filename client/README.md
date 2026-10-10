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

## 登录

正常路径是**扫烽火台（`../server/console`）的「骑手接入」面板给出的二维码**：

```
cd ../server/console && npm run dev     # 或者直接开 server 起的控制台，进「骑手接入」页
# 建号 → 二维码 / 连接串 → 骑手扫一下，地址和账号密码一次填好
```

二维码里是 `ridersim://login?host=…&port=…&name=…&password=…`。解析在
[`lib/login_link.dart`](lib/login_link.dart)，**跟服务端 `server/src/net/join.rs`
是一对**，改一个必须改另一个（契约在 `packages/protocol.json` 的 `login_uri`）。

也可以手填「服务器地址 + 端口 + 骑手名字 + 密码」。默认地址 `ws://127.0.0.1:8080/ws`。

**令牌是开发后门**，藏在登录页「高级」里：

| 令牌 | 效果 |
|---|---|
| `rider:1` | 绑定到 1 号骑手（内置模拟骑手，可直接跑） |
| `new:张三` | 现场建一个真人骑手（绕过账号表，不查重） |
| 留空 | 观战 |

> 扫码用的是 `mobile_scanner`（相机是原生能力）。`client/android` 还没生成之前
> ——也就是还没跑上面那条 `flutter create` 之前——**扫码页在真机上跑不起来**，
> 用登录页的「粘贴」把手填走通即可。

## 目录

```
lib/
  protocol.dart          # 协议常量 + 数据模型（与 protocol.json 一一对应）
  login_link.dart        # 解析 ridersim://login?... 连接串（服务端 join.rs 的对面）
  game_state.dart        # 客户端世界状态：吃事件、发通知、驱动 UI
  game_client.dart       # WebSocket 连接、握手、自动重连、上行指令
  screens/
    login_page.dart      # 扫码 + 手填地址/名字/密码（令牌后门折在「高级」里）
    scan_login_page.dart # 扫码页（mobile_scanner，需要原生工程）
    rider_home.dart      # 主界面：大厅 / 我的单 / 战报
  widgets/
    order_card.dart      # 订单卡片（含抢单倒计时）
test/
  login_link_test.dart   # 连接串解析，含与服务端逐字对齐的用例
```

## 登录被拒时会发生什么

服务端握手失败会先发一帧 `error` 再断开。客户端**不会傻等重连**：
`welcome` 之前收到的任何 `error` 都当握手失败，退回登录页把原因显示出来
（`NO_ACCOUNT` → 还没建号；`BAD_CREDENTIALS` → 名字或密码不对）。
`welcome` 之后的报错是跑单过程中的正常反馈，不断线。

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

- **相机扫码没在真机上验过**：`mobile_scanner` 要原生工程（`client/android` 还没生成），
  本机也没有 Flutter。桌面 / web 上不开扫码页，用「粘贴」走手填。
- 没有「改密码」的界面。协议（`change_password`）与 `GameClient.changePassword`
  都通了，缺的只是一个入口——但这是把二维码里明文密码作废的唯一手段，应该尽快补。
- 地图：目前只有列表，没有高德图层与轨迹回放。
- 抢单窗口倒计时用的是服务端下发的时间戳插值，未做本地校时。
- 离线缓存、推送、埋点都没做。

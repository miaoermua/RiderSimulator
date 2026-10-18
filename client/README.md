# 骑手端（Flutter）

骑手模拟器的真人骑手客户端。与服务端通过 WebSocket 通信，协议见
[`../packages/protocol.json`](../packages/protocol.json)。

## 跑起来

```bash
# 1. 先起服务端
cd ../server && RIDER_SIM_ALLOW_CONTROL=true cargo run

# 2. 再起客户端
flutter pub get
flutter run            # Android / iOS（真机或模拟器）或 Chrome；没有桌面平台目录
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

> 扫码用的是 `mobile_scanner`（相机是原生能力）。`client/android/`、`client/ios/`、
> `client/web/` **都已生成**，真机 / 模拟器可以直接 `flutter run`；但**没有**
> `macos/` 目录，跑不了 macOS 桌面，而 web 又不给相机——那种场合用登录页的
> 「粘贴」把手填走通即可。

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
    rider_home.dart      # 主界面：待抢 / 待取 / 待送 三个 Tab
  widgets/
    order_card.dart      # 订单卡片（含抢单倒计时）
test/
  login_link_test.dart   # 连接串解析，含与服务端逐字对齐的用例
```

上面只列了主线上的几个文件。实际规模：`lib/` 顶层 7 个 dart 文件、`screens/` 11 个页面、
`widgets/` 8 个组件、`map/` 2 个文件，完整清单见 [`../docs/实现现状.md`](../docs/实现现状.md) §六。

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

三个平台目录**都已生成**，包名也已按上面的值改到位（2026-10-18 逐处核对）：

| 文件 | 现值 |
|---|---|
| `android/app/build.gradle.kts:8` 与 `:19` | `namespace` 和 `defaultConfig.applicationId` 都是 `net.miaoer.game.crowdsource` |
| `android/app/src/main/kotlin/net/miaoer/game/crowdsource/MainActivity.kt:1` | `package net.miaoer.game.crowdsource` |
| `ios/Runner.xcodeproj/project.pbxproj:386` 与 `:402` | `PRODUCT_BUNDLE_IDENTIFIER` 两处：`net.miaoer.game.crowdsource`、`net.miaoer.game.crowdsource.RunnerTests` |
| `android/app/src/main/AndroidManifest.xml:14` | `android:label="骑手端"`，这是**应用显示名**（中文不受包名影响），跟包名两回事 |

`namespace` 必须和 `MainActivity.kt` 的 `package` 一致，否则清单里的 `.MainActivity` 解析不到。

当年生成用的命令（留作备查；`--org` 只是前缀，模板给出的是
`net.miaoer.game.rider_client`，包名仍需按上表手改）：

```bash
cd client
flutter create --org net.miaoer.game --project-name rider_client \
  --platforms=android,ios,web .
```

`--project-name` 保持 `rider_client`，Dart 包名不变。原方案里还带了 `macos`，
但**没有生成**，所以现在跑不了 macOS 桌面。

## 还没做的

这是**骨架**，联调通了「登录 → 看大厅 → 抢单 → 到店/送达 → 任务对话 → 看战报」这条主线：

- **相机扫码没在真机上验过**：原生工程已生成，但本机只跑过静态检查
  （`flutter analyze` 干净、`flutter test` 13 个通过），没上过设备。
  桌面 / web 开不了扫码页，用「粘贴」走手填。
- 没有「改密码」的界面。协议（`change_password`）与 `GameClient.changePassword`
  都通了，缺的只是一个入口——但这是把二维码里明文密码作废的唯一手段，应该尽快补。
- 地图：`widgets/delivery_map.dart`（`flutter_map` + 高德 / OSM 瓦片、取送连线）
  已用在订单详情、路线规划、指派页和订单卡上；**轨迹回放没有**——客户端完全不读
  `trajectory`。
- 抢单窗口倒计时用的是服务端下发的时间戳插值，未做本地校时。
- 离线缓存、推送、埋点都没做。

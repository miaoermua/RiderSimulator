# AGENTS.md

给在本仓库工作的 AI 编码代理的项目指引。人类读者也可以看，但下面的约定主要是
**代理容易踩的坑**。开始改代码之前先读这一份。

## 一、三个交付物

| 交付物 | 位置 | 技术栈 |
|---|---|---|
| 服务端（仿真引擎 + WS Hub + HTTP API） | `server/` | Rust |
| 烽火台控制台 | `server/console/` | React 18.3 + TypeScript 5.6 + Ant Design 5.22 + Leaflet 1.9 + Vite 6 |
| 骑手端 | `client/` | Flutter（`client/pubspec.yaml` 当前版本 0.2.3+4） |

控制台的构建产物 `server/static/console.html` 由 `include_str!` 编进 Rust 二进制
（`server/src/net/ws.rs:85`），**不是**运行时去磁盘读的静态文件。改了控制台源码
但没重新 `npm run build`，二进制里跑的还是旧页面。

## 二、命令

```bash
# Rust —— 需要 cargo 在 PATH 里
export PATH="$HOME/.cargo/bin:$PATH"
cd server
cargo test --workspace          # 全量测试
cargo fmt --all --check         # 格式检查
cargo clippy --all-targets      # lint

# 控制台
cd server/console
npm run typecheck               # tsc --noEmit
npm run build                   # tsc --noEmit && vite build，产物落到 ../static/
npm run dev                     # 本地开发，代理 /ws /api /healthz 到 127.0.0.1:8080

# 骑手端
cd client
flutter analyze
flutter run
```

跑起来的最小路径见 `docs/开发指南.md`；接口清单见 `docs/API.md`。

## 三、两条硬约束（由测试强制，不是文档口头约定）

`server/tests/layering.rs` 会**读源码文件**并断言：

1. `server/src/sim/` **零 IO** —— 不许碰 `fs`、网络、数据库。
2. `server/src/sim/` **不依赖 `human/`**。

这个测试文件本身的注释写得很清楚：写在文档里的约束没人会记得，所以要让它们在
测试里红。往 `sim/` 里加任何 IO 或 `human/` 依赖都会让 `cargo test` 失败——
**不要为了让测试过而去放宽 `layering.rs`**，那是把守卫拆掉。

`sim/` 需要外部数据时，走 `TickInput` 一类的入参注入，由 `net/` 或 `app/` 去取。

## 四、契约同步点（改一处必须同时改其余）

本项目**没有**共享类型代码生成，契约靠人肉同步：

- **WS / HTTP 协议**
  `packages/protocol.json` ↔ `server/src/net/protocol.rs` ↔ `client/lib/protocol.dart`
  ↔ `server/console/src/protocol.ts`
- **登录二维码**
  权威实现在 `server/src/net/join.rs` 的 `JoinInfo::login_uri`，客户端解析在
  `client/lib/login_link.dart`，格式 `ridersim://login?host=&port=&name=&password=`。
  两边必须一致；`packages/protocol.json` 的 `login_uri` 是这份约定的文字记录。

> ⚠️ `packages/protocol.json` 目前**落后于代码**，不能当作完整清单使用。实测：
> `ClientMessage` 有 36 个变体，契约只记录 23 个；`SimEvent` 有 23 个变体，契约只
> 记录 17 个；HTTP 侧 18 条 `/api` 路径（23 个方法-路径组合）中，契约的 `http_api`
> 只写了 5 条路径（8 个方法-路径组合），且缺 `state` / `dispatch` / `reassign` /
> `config` / `riders/{id}/*` / `ai*` / `activities*`。
> 以代码为准，完整清单见 `docs/API.md`，缺口明细见 `docs/实现现状.md` §四。

新增/改名消息类型时，把上面 4 处（登录串再加 2 处）都过一遍，别只改服务端。

## 五、坐标系必须成对切换

模拟世界内部**永远是 GCJ-02**。`coord_system` 只决定出口转换：GCJ-02 配高德瓦片，
WGS-84 配 OSM 瓦片。换坐标系不换底图（或反之）会让所有点位整体偏几百米。

手机上报的 GPS 是 WGS-84，**必须在客户端先换算**再发 `set_position`；直接塞原始值
一样会偏。相关：`server/src/net/api.rs` 的 `RIDER_POSITION_PATH` 说明、
`client/lib/screens/running_settings_sheet.dart` 里的底图选择器。

## 六、运行期 LLM 不会自己开

`RIDER_SIM_LLM=true` 是**唯一**会在启动时自动启用 brain 的开关。从控制台设的 Key
（`set_llm_config`）不会让 brain 在下次启动时自动打开——这是刻意的，避免重启
悄无声息地烧掉配额。别"顺手"把它改成自动启用。

控制台设 Key 之后服务端只会通过 `server_info` 回显 `llm_configured` /
`llm_base_url` / `llm_model`，**永不回显 Key 本身**。往任何日志、事件、快照里
加字段时注意不要把 key 带出去。

## 七、其他容易误判的点

- **`server/src/task/` 是空目录。** 任务对话的代码在 `server/src/sim/task.rs`。
  看到空目录不要往里塞东西，也不要据此推断"任务功能没实现"。
- **钱包允许为负。** `economy::debit` 不再把 `cash_cents` 钳在 0。罚单超过当日收入
  时骑手就是倒欠平台，负余额是真实状态，不是 bug，别加"兜底归零"。
- **改派只在 `assigned` 阶段可用。** 一旦 `picked_up`，餐损与保温已随原骑手走，
  服务端会回 `WRONG_PHASE`。控制台因此不画这个按钮——别"补全"成一直可见。
- **强制指派会立刻改世界状态。** `scheduler::assign_forced` 先挂单再置
  `assign_pending`，接受 / 拒绝只清标记。保持状态自洽优先于"等骑手确认再落地"。
- **拒单额度用尽时，到点是自动收下，不是自动拒。** 客户端 `GameState.assignWindow`
  是 30 真实秒；额度没了就自动 accept。
- **顶栏是唯一订阅 tick 的组件。** `server/console/src/components/TopBar.tsx` 在
  60x 档每 16ms 重渲染一次，里面只能放便宜的东西。地图和列表读的是**快照**，
  不要把它们改成订阅 tick。
- **`docs/cache/` 整个目录在 `.gitignore` 里。** 里面的设计稿、竞品调研、agent 缓存
  都不入库、不随仓库分发。不要把面向读者的内容只写在那里。

## 八、文档保真约定

本仓库的文档**只写代码里真实存在的东西**：

- 引用符号、路径、常量、命令、错误码之前，先 grep / read 核实，能定位就给
  `file:line`。不确定就别写。
- 不要为了让文档"看起来完整"而补未实现的设计。
- 设计文档（`docs/技术设计.md`、`docs/玩法设计.md`）描述**意图**，
  实现状态记在 `docs/实现现状.md`。两类内容不要混写。
- 发现文档过期（版本号、目录树、路由清单）先核实再改，不要凭印象改。
  `docs/技术设计.md` 的 §二 目录结构是**尚未跟进代码的设计原稿**（`desktop/`、
  `rider/` 等都不存在），**没有代为改写**；实际结构看 `docs/实现现状.md` §六。
  要动那一节属于结构调整，先跟维护者确认。

## 九、当前已知的失败项

- `server/tests/acceptance.rs` 的
  `speed_multiplier_only_changes_real_time_pacing` **当前失败**。
  根因：`SimConfig::default()` 的 `realtime` 为 `true`，而该测试没有先关掉实时档，
  于是 `tick_interval_ms()` 返回 `REALTIME_TICK_MS`（60000）而不是断言的 1000。
  这是**过期测试**，不是产品缺陷——`server/src/sim/config.rs:616` 的
  `tick_interval_scales` 单元测试就正确地先设了 `c.realtime = false`。
  修它属于改测试代码，动手前先跟维护者确认，别把它跟你的功能改动混在一个提交里。

# RiderSimulator

重生之模拟器，你可以通过后台或者 agent 给你或者你的朋友下发任务跑单。

一个外卖配送调度的**仿真 / 演示程序**：虚构城市、虚构商户、虚构骑手，
跑真实的派单算法与运营流程。与任何真实平台无关，
来源与用途边界见 [`DISCLAIMER.md`](DISCLAIMER.md)。

## 三个交付物

| | 位置 | 技术栈 |
|---|---|---|
| 服务端（仿真引擎 + WS Hub + HTTP API） | `server/` | Rust |
| 烽火台控制台 | `server/console/` | React 18 + Ant Design 5 + Leaflet + Vite |
| 骑手端 | `client/` | Flutter（Android / iOS / Web 工程均已生成） |

## 快速开始

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd server
RIDER_SIM_ALLOW_CONTROL=true cargo run
```

浏览器打开 http://127.0.0.1:8080/ —— 控制台已编进二进制，不用另起前端。
骑手端 `cd client && flutter pub get && flutter run`。
完整步骤、环境变量与常见卡点见 [`docs/开发指南.md`](docs/开发指南.md)。

## 已经实现

- 算法骑手，算法后台，算法站长
- llm 决策层（骑手 / 站长 / 烽火台三层，含人工否决与决策流）
- 骑手端应用框架（`client/android`、`client/ios`、`client/web` 三端工程齐备）
- map（骑手端 flutter_map，控制台 Leaflet；GCJ-02 / WGS-84 双坐标系）
- 合并单，指派单（含强制派单确认流与每日拒单额度）
- 人员管理（账号体系、扫码登录、改名、上下线）
- 钱包（余额 / 流水 / 罚单 / 时薪 / 到手，允许为负）
- 经济系统（起步价、里程阶梯、等级加成、高峰溢价、雨补、抽水、重投补贴）
- 天气剧本与速度风险系统（超速、事故、疲劳、情绪与摆烂）
- 任务对话与末端交付会话
- 平台活动与首页通知
- 实时档与 1/2/4/8/10/60x 变速
- 日结流水 CLI（`server/src/bin/ledger.rs`）
- 供外部工具驱动派单的 HTTP API 与 skill（`.codebuddy/skills/rider-dispatch/`）

各项的**实测验证结果与已知缺口**记在 [`docs/实现现状.md`](docs/实现现状.md)，
包括一个当前失败的验收测试（已定位根因，非产品缺陷）。

## 文档

| 文件 | 讲什么 |
|---|---|
| [`docs/README.md`](docs/README.md) | 文档导航 |
| [`docs/技术设计.md`](docs/技术设计.md) | 架构、协议、内核设计（意图） |
| [`docs/玩法设计.md`](docs/玩法设计.md) | 经济、天气、订单、AI、界面规格（意图） |
| [`docs/实现现状.md`](docs/实现现状.md) | 代码此刻的真实状态与缺口（事实） |
| [`docs/API.md`](docs/API.md) | HTTP 路由全表、WS 入口、鉴权、环境变量 |
| [`docs/开发指南.md`](docs/开发指南.md) | 环境、跑起来、验证命令 |
| [`AGENTS.md`](AGENTS.md) | 给 AI 编码代理的约定与陷阱 |
| [`DISCLAIMER.md`](DISCLAIMER.md) | 独立实现声明、人身安全与保险边界、GPL 许可范围、免责 |

`docs/cache/` 是设计过程稿与调研，已在 `.gitignore` 中，不随仓库分发。

## 许可与用途

GPL-3.0，见 [`LICENSE`](LICENSE)。本项目仅供技术演示、玩法探索与个人娱乐，
**不具备生产环境价值**（无生产级安全、无资金能力、无真实调度数据），
且全部为独立实现、未对任何现有平台做逆向工程 ——
详见 [`DISCLAIMER.md`](DISCLAIMER.md)。

> ⚠️ **不要边骑车边看屏幕，也不要照着它去现实里跑单。**
> 本项目是纯软件仿真，**不具备任何保险能力**：不代购、不承保、不垫付任何
> 人身意外险、雇主责任险或第三者责任险。现实骑行与配送过程中产生的一切问题
> （含交通意外、人身伤害、财产损失与第三方索赔），由行为人自行承担，
> 与作者无关。详见 [`DISCLAIMER.md` §八](DISCLAIMER.md#八人身安全与保险)。

# 接口清单

> 本文是**路由与鉴权的总览**，从代码如实抽取。
> 请求体 / 响应体的字段定义不在此重复——以 `packages/protocol.json` 和源码为准
> （注意该契约存在滞后，见 `docs/实现现状.md` §四）。
>
> 事实来源：`server/src/net/api.rs`（路径常量 40–87 行、`routes()` 339–370 行）、
> `server/src/net/ws.rs`（82–100 行）。最后核实：2026-10-18。

---

## 一、非 `/api` 入口

| 方法 | 路径 | 说明 | 代码位置 |
|---|---|---|---|
| `GET` | `/` | 烽火台控制台。单文件 HTML，由 `include_str!` 编进二进制 | `ws.rs:82,85,98` |
| `GET` | `/ws` | WebSocket 升级。文本帧，JSON，单帧上限 65536 字节 | `ws.rs:41,99` |
| `GET` | `/healthz` | 健康检查 | `ws.rs:43,100` |

控制台产物由 `server/console` 构建后写入 `server/static/console.html`，
**修改控制台源码需重新构建**才会进二进制。

---

## 二、鉴权

HTTP 与 WS 共用一套控制端鉴权（`api.rs:89–126`）：

| 情形 | 行为 |
|---|---|
| 配了 `RIDER_SIM_CONTROL_TOKEN` | 必须带 `Authorization: Bearer <token>`；缺失或不符 → `401 UNAUTHORIZED`（"控制端令牌无效"） |
| 未配 token，且 `RIDER_SIM_ALLOW_CONTROL=true` | 直接放行 |
| 未配 token，且未开 `ALLOW_CONTROL` | `403 CONTROL_DISABLED`（"服务端未开控制端：请设置 RIDER_SIM_CONTROL_TOKEN"） |

WS 侧的控制端令牌写在 `hello` 消息里：`hello.token = "control:<令牌>"`
（见 `server/src/main.rs:15` 与 `packages/protocol.json` 的 `roles`）。

**错误体一律** `{"error":{"code","message"}}`。

---

## 三、`/api` 路由全表

18 条路径，23 个方法-路径组合。说明取自 `api.rs` 中各路径常量的文档注释。

### 观测

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/api/state` | 读当前世界（控制端视角完整快照） |
| `GET` | `/api/ai` | 读三层 AI 状态 |
| `GET` | `/api/ai/state` | 同上，等价路径，留给只认显式路径的调用方 |

### 调度

| 方法 | 路径 | 说明 |
|---|---|---|
| `POST` | `/api/dispatch` | 派单：把**大厅里无主**的单指派给骑手。整批按顺序执行，一条失败不挡后面 |
| `POST` | `/api/reassign` | 改派：把**别人手上还没取到餐**的单转给另一个骑手。整批同上 |
| `POST` | `/api/config` | 批量改控制端开关 |
| `POST` | `/api/riders/{id}/online` | 强制某骑手上线 / 下线 |
| `POST` | `/api/riders/{id}/position` | 校准某骑手坐标（`{"lat":..,"lng":..}`）。**只改坐标，不动状态机** |

`dispatch` 与 `reassign` 刻意分成两个接口而非合并：两者前置条件正相反，失败原因
也不同（`ALREADY_CLAIMED` vs `WRONG_PHASE`），合成一个调用方就分不清自己错在哪。
详见 `api.rs:43–48` 的注释。

`riders/{id}/position` 的坐标范围在 HTTP 层校验，业务层再兜底一次（HTTP 层可被绕过）。

### AI

| 方法 | 路径 | 说明 |
|---|---|---|
| `POST` | `/api/ai` | 改 AI 开关 / 托管模式 |
| `POST` | `/api/ai/config` | 同上，等价路径 |
| `POST` | `/api/ai/reject` | 人工否决一条 AI 决策 |

### 骑手账号

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/api/accounts` | 列账号名单（含手机可达地址） |
| `POST` | `/api/accounts` | 建号 |
| `POST` | `/api/accounts/{name}/password` | 改 / 重置密码。`password` 缺省时服务端生成一个并回显 |
| `DELETE` | `/api/accounts/{name}` | 删账号 |

**建号响应里带明文密码和二维码**——这是全系统唯一一次能看到明文的窗口，
库里只存 Argon2 哈希（`api.rs:63–67`，实现见 `server/src/net/join.rs`）。

### 接入地址

| 方法 | 路径 | 说明 |
|---|---|---|
| `POST` | `/api/join` | 换「手机该连哪儿」（`{"addr":"https://…"}`；`null` 或空串 = 恢复环境变量或自动探测） |

**只在本次运行内有效，不落库**：Cloudflared 每次给的域名不同，存库只会让下次
启动拿着一个已失效的地址生成二维码（`api.rs:72–77`）。

### 平台活动

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/api/activities` | 列全部活动（含已下架） |
| `POST` | `/api/activities` | 新建活动 |
| `POST` | `/api/activities/{id}/active` | 上架 / 下架（`{"active": false}`） |
| `DELETE` | `/api/activities/{id}` | 删除活动 |

`reward_cents` 单位是**分**（控制台按元填表，转换后再发）。
`target_orders` 必须 > 0，否则"跑 0 单给钱"会立刻给所有在线骑手发钱。

### 首页通知

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/api/notice` | 读通知 |
| `POST` | `/api/notice` | 写通知（覆盖） |
| `DELETE` | `/api/notice` | 清空 |

全局只有一条通知，改了就覆盖，没有历史版本（设计取舍见 `server/src/sim/notice.rs`）。

---

## 四、环境变量

启动配置全部走环境变量，无配置文件（`server/src/main.rs:10–19`）：

| 变量 | 默认 | 作用 |
|---|---|---|
| `RIDER_SIM_ADDR` | `127.0.0.1:8080` | 监听地址 |
| `RIDER_SIM_DB` | `rider_sim.db` | SQLite 路径；打不开自动降级内存库 |
| `RIDER_SIM_DATA` | 自动查找 `data/` | 剧本目录 |
| `RIDER_SIM_SEED` | 内置 | 随机种子 |
| `RIDER_SIM_ALLOW_CONTROL` | `false` | 无 token 时能否拿控制权（WS 与 HTTP 共用） |
| `RIDER_SIM_CONTROL_TOKEN` | 无 | 控制端令牌 |
| `RIDER_SIM_LLM` | `false` | 是否启用 LLM（无 Key 时自动关闭） |
| `RIDER_SIM_MAX_CONNECTIONS` | `64` | 同时在线上限 |
| `RIDER_SIM_PUBLIC_ADDR` | 自动探测 | 烽火台告知骑手「连哪个地址」用的主机 |
| `RIDER_SIM_PUBLIC_URL` | 无 | 手机该连的**完整**地址；配了就以它为准，二维码与客户端原样使用 |
| `RUST_LOG` | `info` | 日志级别 |

---

## 五、WS 消息总览

帧格式与字段不在本文维护。权威位置：

| 内容 | 位置 |
|---|---|
| 契约文字记录 | `packages/protocol.json`（`protocol_version: 1.0`） |
| 服务端实现 | `server/src/net/protocol.rs`（`ClientMessage` 枚举，41 行起） |
| 骑手端类型 | `client/lib/protocol.dart` |
| 控制台类型 | `server/console/src/protocol.ts` |
| 角色 | `rider` / `control` / `spectator`，按角色过滤下发 |
| 登录二维码 | `ridersim://login?host=&port=&name=&password=`，权威实现 `server/src/net/join.rs`，客户端解析 `client/lib/login_link.dart` |

`ClientMessage` 共 36 个变体、`SimEvent` 共 23 个变体；`packages/protocol.json` 只
分别记录了 23 与 17 个，差额清单见 `docs/实现现状.md` §四。

---

## 六、外部工具

`.codebuddy/skills/rider-dispatch/` 提供了一套供外部工具（如 workbuddy）通过 HTTP
驱动派单的 skill 与 `references/api.md` + `scripts/dispatch.py`。该目录随仓库分发，
但定位是**代理技能缓存**，不是本项目对外承诺的稳定接口。

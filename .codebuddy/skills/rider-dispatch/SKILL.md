---
name: rider-dispatch
description: 通过 RiderSimulator 的远程 HTTP 接口给骑手派单（调单 / 塞单 / 改派）。当用户或上游工具（workbuddy 等）需要把某个订单指派给某个骑手、批量调度大厅里的待分配订单、或读取骑手与订单现状来做派单决策时使用。触发词：派单、调单、塞单、把单给某人、dispatch、assign order。
---

# 骑手模拟器派单

## 用途

RiderSimulator 服务端跑着一个不停 tick 的世界：订单进大厅、骑手抢单、AI 兜底。
本 skill 教你怎么用 **HTTP 接口** 从外部介入，把订单**指定给某个人**，而不是
等着算法派。接口与浏览器里的烽火台控制台共用同一条执行队列，所以外部派单和
手点按钮的效果完全一致：骑手端会收到强制派单、控制台会看到决策记录。

## 何时使用

- 用户说「把 42 号单给老王」「这一片让 r3 去跑」「这批单重新分一下」
- workbuddy 之类的上游工具调这个接口做人工/算法混合调度
- 需要先读一遍 `谁在线、谁手上有什么单、大厅还剩哪些单` 再决定怎么分

## 前置条件

服务端必须开着控制端，否则所有接口返回 `403 CONTROL_DISABLED`。启动时给令牌：

```bash
RIDER_SIM_CONTROL_TOKEN=<令牌> cargo run          # 服务端，默认 0.0.0.0:8080
```

之后每个请求都带上：

```bash
-H 'Authorization: Bearer <令牌>'
```

基地址按部署填，本地默认 `http://127.0.0.1:8080`。所有请求与响应都是 JSON。
完整字段与错误码见 `references/api.md`。

## 工作流

### 第 1 步：读现状

```bash
curl -s -H 'Authorization: Bearer <令牌>' http://127.0.0.1:8080/api/state
```

从响应里挑三样东西：

| 字段 | 用法 |
|---|---|
| `hall` | 待分配订单（`status=pending`）。要在 `claim_deadline_min` 之前动手，否则被系统或 AI 拿走 |
| `riders[]` | 每个骑手的 `id`、`key`（形如 `r3`）、`online`、`held`、`lat`/`lng`、`phase`、`fatigue` |
| `active_orders[]` | 已在跑的单，含 `assigned_to`、商家/顾客坐标、`promised_minutes`、`fee.takehome_cents` |

挑人时的硬约束（服务端会拦，提前避开能少一次失败）：

- `online` 必须为 `true`
- `held` 数量必须小于 `config.backorder_max`
- 不能是 `injured` / `frozen`
- 订单必须是 `pending` 或 `expired`，且不能在窗口外太久

判断「顺路」看 `held` 里那几单的顾客坐标和当前 `lat`/`lng`，取方向一致、绕路少的骑手。

### 第 2 步：派单

```bash
curl -s -X POST -H 'Authorization: Bearer <令牌>' -H 'content-type: application/json' \
  -d '{"assignments":[{"rider_id":"r3","order_id":42,"reason":"顺路，手上单就在隔壁"}]}' \
  http://127.0.0.1:8080/api/dispatch
```

`rider_id` 写 `"r3"` 或数字 `3` 都行。`reason` 可选，会出现在控制台的决策记录里，
**建议总是写**——事后复盘全靠它。

三种请求体都收：`{"assignments":[...]}`、单条对象、裸数组。

### 第 3 步：核对结果

`dispatch` 永远返回 200，逐条报告成败。**一条失败不影响后面的**，所以批量派单
必须读 `results` 而不是只看 HTTP 状态码：

```json
{
  "applied": 1,
  "failed": 1,
  "results": [
    {"order_id": 42, "rider_id": 3, "rider_key": "r3", "ok": true,
     "planned_minutes": 38, "reason": "顺路，手上单就在隔壁"},
    {"order_id": 43, "rider_id": 7, "rider_key": "r7", "ok": false,
     "code": "RIDER_OFFLINE", "message": "该骑手当前离线"}
  ]
}
```

失败的常见 `code`：`RIDER_NOT_FOUND`（解析不出 key）、`RIDER_OFFLINE`、
`ALREADY_CLAIMED`（已被抢/已派）、`ORDER_EXPIRED`、`HELD_LIMIT_REACHED`、
`RIDER_INJURED`。

有 `failed` 就按 code 换人重试，不要盲目重发同一条（`ALREADY_CLAIMED` 重发
不会成功）。

## 其他常用操作

```bash
# 改开关：抢单窗口秒数、背单上限、是否允许超速……越界值服务端自动夹取
curl -s -X POST -H 'Authorization: Bearer <令牌>' -H 'content-type: application/json' \
  -d '{"claim_window_secs":5,"backorder_max":2,"allow_speeding":true}' \
  http://127.0.0.1:8080/api/config

# 立刻让某个骑手上线 / 下线（手上有单时不能下线）
curl -s -X POST -H 'Authorization: Bearer <令牌>' -H 'content-type: application/json' \
  -d '{"online":false}' http://127.0.0.1:8080/api/riders/2/online
```

## 用脚本

`scripts/dispatch.py` 封装了上述调用，适合给不便拼 curl 的工具直接用：

```bash
# 看大厅 + 在线骑手
python3 scripts/dispatch.py state
# 派单
python3 scripts/dispatch.py assign --order 42 --rider r3 --reason "顺路"
# 批量：从标准输入读 {"assignments":[...]}
python3 scripts/dispatch.py assign --json '{"assignments":[{"rider_id":3,"order_id":42}]}'
```

环境变量 `RIDER_SIM_BASE`（默认 `http://127.0.0.1:8080`）与
`RIDER_SIM_CONTROL_TOKEN`（必需）。脚本失败时以非 0 退出码结束。

## 注意

- 派单是**立即生效**的写操作，没有二次确认，也没有撤销。改主意只能等骑手拒单
  或让订单超时。
- 别拿同一个 `order_id` 连发。先看 `results[i].code` 再决定。
- 未超时且未被抢的订单优先；`expired` 的订单能被强制指派救回来，但骑手端
  的时限压力已经形成。
- 演练或批量调度前，先用 `POST /api/config` 把 `claim_window_secs` 调大，
  给自己留出决策时间。

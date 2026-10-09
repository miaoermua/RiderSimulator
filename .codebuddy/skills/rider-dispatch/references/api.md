# 远程控制接口参考

基地址写作 `<BASE>`（本地默认 `http://127.0.0.1:8080`）。
除 `/healthz` 外全部需要鉴权。

## 鉴权

```
Authorization: Bearer <RIDER_SIM_CONTROL_TOKEN>
```

服务端判定顺序：

1. 配了 `RIDER_SIM_CONTROL_TOKEN` → 令牌必须逐字节相等
2. 没配令牌但 `RIDER_SIM_ALLOW_CONTROL=true` → 放行（开发态）
3. 两者都没有 → `403 CONTROL_DISABLED`

失败响应体：

```json
{"error": {"code": "UNAUTHORIZED", "message": "控制端令牌无效"}}
```

## GET /api/state

读控制端视角的完整快照。等价于 WebSocket 的 `welcome`，另外多一个 `server_info`。

响应顶层字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `protocol_version` | int | 协议版本 |
| `day` | int | 第几天（从 1 起） |
| `sim_minute` | int | 当天第几分钟，`0..1440` |
| `elapsed_min` | int | 开局到现在的总分钟 |
| `weather` | string | `clear` / `rain` / `heavy_rain` / `heat` / `cold` / `smog` |
| `weather_label` | string | 中文标签 |
| `weather_speed_factor` | float | 天气对速度的乘数 |
| `rain_bonus_cents` | int | 当前每单雨天补贴（分） |
| `paused`（在 `config` 内） | bool | 是否暂停 |
| `claim_window_ms` | int | 当前抢单窗口毫秒数 |
| `config` | object | 见下 |
| `stats` | object | 运行统计 |
| `server_info` | object | `seed`、`tick_interval_ms`、`llm_enabled`、`amap_enabled`、`hub`（连接数）、`stats` |
| `riders` | array | 骑手列表，**控制端版本**（含收入/疲劳/期望值） |
| `merchants` | array | 商户精简信息 |
| `hall` | array | 待分配订单 |
| `active_orders` | array | 进行中的订单 |
| `last_day_ledgers` | array | 昨日结算（无则空数组） |

### riders[]

```json
{
  "id": 1, "key": "r1", "name": "AI-A", "kind": "simulated",
  "online": true, "injured": false, "frozen": false,
  "speed_mode": "compliant", "level": 1, "rating": 4.8, "completed": 0,
  "held": [1], "phase": "waiting_food", "lat": 30.51725, "lng": 114.3817,
  "hourly_wage_cents": 0, "takehome_cents": 0, "cash_cents": 0, "fines_cents": 0,
  "fatigue": 0.042, "expected_value_cents": null
}
```

- `held` 是订单 id 数组，长度就是背单数——派单前必看。
- `phase` 为 `null` 表示手上没单。
- `kind`：`human` / `simulated`。
- `takehome_cents` / `fatigue` / `expected_value_cents` **只在对控制端下发时有值**。
  对骑手本人下发时换成 `hourly_wage_cents` / `cash_cents` / `fines_cents`。

### orders（`hall` 与 `active_orders` 同构）

```json
{
  "id": 1, "status": "assigned", "tier": "normal", "forced": true,
  "merchant_id": 7, "merchant_name": "东北水饺（鲁巷店）",
  "merchant_lat": 30.5024, "merchant_lng": 114.4067,
  "customer_name": "顾客6434", "customer_address": "…附近 · 27号楼",
  "customer_lat": 30.518476, "customer_lng": 114.36576,
  "distance_m": 5388, "prep_minutes": 16, "promised_minutes": 40,
  "created_at_min": 3, "claim_deadline_min": 6,
  "picked_up_at_min": null, "delivered_at_min": null,
  "assigned_to": 1, "urge_count": 0, "remark": "放门口，不要敲门",
  "back_order": false, "back_order_bonus_cents": 0,
  "fee": {
    "base_fee_cents": 350, "distance_extra_cents": 350, "rain_bonus_cents": 0,
    "level_bonus_cents": 0, "commission_cents": 0, "surge_multiplier": 1.0,
    "quoted_fee_cents": 700, "takehome_cents": 700
  },
  "task": null
}
```

- `status`：`pending`（大厅）→ `assigned` → `picked_up` → `delivered`；
  另有 `expired`（超时）。
- `claim_deadline_min`：大厅单的抢单窗口截止时间（分钟）。
- `fee.takehome_cents` 是骑手实际到手。

### config

| 字段 | 说明 | 可写（`/api/config`） |
|---|---|---|
| `claim_window_secs` | 抢单窗口秒数，夹取到 1~10 | 是 |
| `backorder_enabled` | 是否允许背单 | 是 |
| `backorder_max` | 背单上限，夹取到 1~5 | 是 |
| `allow_speeding` | 允许 `tolerant` 档 | 是 |
| `allow_unrestricted` | 允许 `unrestricted` 档 | 是 |
| `discriminative_commission` | 差异化抽成 | 是 |
| `show_expected_value` | 骑手端显示期望值 | 是 |
| `ai_batch_enabled` | AI 批量派单开关 | 是 |
| `sim_rider_count` | AI 骑手数量，夹取到 0~8 | 是 |
| `max_orders_per_day` | 每日单量上限，`0` = 不限 | 是 |
| `seed` | 随机种子 | 否（只读） |
| `speed_multiplier` | 时间倍速 | 否（走 WS） |
| `paused` | 暂停 | 否（走 WS） |
| `is_peak` / `surge_multiplier` | 高峰与溢价 | 否（自动） |

> 关掉 `allow_speeding` / `allow_unrestricted` 时，服务端会把不合规档位**
> 自动降回 `compliant`**，并在事件流里通知。

## POST /api/dispatch

派单。请求体三种写法：

```json
{"assignments": [{"rider_id": "r3", "order_id": 42, "reason": "顺路"}]}
```
```json
{"rider_id": 3, "order_id": 42, "reason": "顺路"}
```
```json
[{"rider_id": 3, "order_id": 42}]
```

`rider_id` 接受 `3`、`"3"`、`"r3"`；`reason` 可选（默认「控制端强制指派」）。
`assignments` 为空数组 → `400 BAD_REQUEST`，不会进主循环。

**成功一律 200**，逐条报告：

```json
{
  "applied": 1,
  "failed": 1,
  "results": [
    {"order_id": 42, "rider_id": 3, "rider_key": "r3", "ok": true,
     "planned_minutes": 38, "reason": "顺路"},
    {"order_id": 43, "rider_id": 7, "rider_key": "r7", "ok": false,
     "code": "RIDER_OFFLINE", "message": "该骑手当前离线"}
  ]
}
```

`planned_minutes` = 取餐 + 等餐 + 送达的预估总时长。

### 业务失败码（`results[].code`）

| code | 含义 |
|---|---|
| `RIDER_NOT_FOUND` | `rider_id` 解析不出来，或骑手 id 不存在 |
| `ORDER_NOT_FOUND` | `order_id` 不存在 |
| `RIDER_OFFLINE` | 骑手已下线 |
| `RIDER_INJURED` | 骑手受伤中 |
| `RIDER_FROZEN` | 骑手被封禁中 |
| `ALREADY_CLAIMED` | 订单已被抢 / 已被派 |
| `ORDER_EXPIRED` | 订单已过期 |
| `HELD_LIMIT_REACHED` | 骑手到达背单上限或当日单量上限 |
| `RATING_TOO_LOW` | 评分不够接该档订单（强制指派不看这条） |

### 强制指派的实际约束

服务端 `assign_forced` 只检查：

1. 订单状态是 `pending` 或 `expired`
2. 骑手 `online == true`
3. 骑手不在受伤状态
4. `held.len() < config.backorder_max`

**刻意绕过**评分门槛与抢单窗口。指派成功后：

- 订单 `forced = true`，`assigned_to` 落到该骑手
- 广播 `order_claimed`（`forced: true`）给相关方
- 广播一条 `ai_decision`，`source = "manual"`，带上你给的 `reason`

## POST /api/config

请求体是**稀疏补丁**，只写要改的字段：

```json
{"claim_window_secs": 5, "backorder_max": 2, "allow_speeding": true}
```

越界值由服务端夹取后生效。响应是夹取后的完整 `config`。
会广播 `config_changed`；若因此把骑手降档或上下线，还会附上对应状态事件。

## POST /api/riders/{id}/online

```json
{"online": false}
```

`online` 缺省为 `true`。手上有单时不能下线，返回
`409 RIDER_NOT_FOUND` / 其它冲突码。响应：

```json
{"rider_id": 2, "online": false}
```

## 通用错误响应

```json
{"error": {"code": "…", "message": "…"}}
```

| HTTP | code | 场景 |
|---|---|---|
| 400 | `BAD_REQUEST` | 请求体不是合法 JSON 或缺必填字段 |
| 401 | `UNAUTHORIZED` | 配了令牌但没带 / 带错 |
| 403 | `CONTROL_DISABLED` | 服务端没开控制端 |
| 409 | 业务码 | 骑手不存在、下单失败等 |
| 503 | `INTERNAL` | 主循环已退出 |

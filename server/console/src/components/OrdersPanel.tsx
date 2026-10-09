import { Badge, Empty, List, Select, Space, Typography } from "antd";
import { STATUS_LABEL } from "../const";
import { useIsControl, useStore } from "../hooks";
import type { Order } from "../protocol";
import { store, yuan } from "../store";
import { Metric, MetricRow } from "./Metric";

/**
 * 大厅待抢 + 在跑订单。
 *
 * 重绘订阅快照（不是 tick），所以 60x 档下这里的下拉框不会被每帧重建，
 * 用户选到一半的指派对象不会丢。
 */
export function OrdersPanel() {
  const snap = useStore((s) => s.snap);
  const isControl = useIsControl();

  if (!snap) return <Empty style={{ marginTop: 48 }} description="等待快照…" />;

  const candidates = snap.riders.filter((r) => r.online && !r.injured && !r.frozen);
  const nameOf = (id: number | null) =>
    id == null ? "-" : (snap.riders.find((r) => r.id === id)?.name ?? `#${id}`);

  return (
    <div className="console-scroll">
      <List
        size="small"
        header="大厅待抢"
        itemLayout="vertical"
        rowKey="id"
        dataSource={snap.hall}
        locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="大厅是空的" /> }}
        style={{ marginBottom: 24 }}
        renderItem={(o) => (
          <List.Item>
            <List.Item.Meta
              title={<OrderTitle order={o} right={<Badge {...ttlBadge(o.claim_deadline_min - snap.elapsed_min, "窗口")} />} />}
              description={
                <MetricRow>
                  <Metric>{(o.distance_m / 1000).toFixed(1)}km</Metric>
                  <Metric>承诺 {o.promised_minutes} 分</Metric>
                  <Metric>抽水 ¥{yuan(o.fee.commission_cents)}</Metric>
                  {o.task && <Metric>任务 {o.task.name}</Metric>}
                </MetricRow>
              }
            />
            {isControl && (
              <Select
                style={{ width: "100%", marginTop: 8 }}
                size="small"
                showSearch
                optionFilterProp="label"
                placeholder="指派给…"
                value={null}
                options={candidates.map((r) => ({
                  value: r.id,
                  label: `${r.name}（${r.held.length} 单）`,
                }))}
                onChange={(v) => store.send("force_assign", { order_id: o.id, rider_id: v })}
              />
            )}
          </List.Item>
        )}
      />

      <List
        size="small"
        header="在跑"
        itemLayout="vertical"
        rowKey="id"
        dataSource={snap.active_orders}
        locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="没有在跑的单" /> }}
        renderItem={(o) => (
          <List.Item>
            <List.Item.Meta
              title={
                <OrderTitle
                  order={o}
                  right={<Badge {...ttlBadge(o.created_at_min + o.promised_minutes - snap.elapsed_min, "剩")} />}
                />
              }
              description={<ActiveMeta order={o} rider={nameOf(o.assigned_to)} />}
            />
          </List.Item>
        )}
      />
    </div>
  );
}

/** 标题行：`#id 商家 → 顾客`，右侧挂状态徽标并被推到最右。 */
function OrderTitle({ order: o, right }: { order: Order; right: React.ReactNode }) {
  return (
    <Space size={8} style={{ width: "100%" }}>
      <Typography.Text strong>#{o.id}</Typography.Text>
      <Typography.Text>{o.merchant_name}</Typography.Text>
      <Metric>→ {o.customer_address}</Metric>
      <span style={{ marginInlineStart: "auto" }}>
        <Space size={8}>
          <Typography.Text strong>¥{yuan(o.fee.quoted_fee_cents)}</Typography.Text>
          {right}
        </Space>
      </span>
    </Space>
  );
}

function ActiveMeta({ order: o, rider }: { order: Order; rider: string }) {
  return (
    <MetricRow>
      <Metric>{STATUS_LABEL[o.status] ?? o.status}</Metric>
      <Metric>骑手 {rider}</Metric>
      <Metric>{(o.distance_m / 1000).toFixed(1)}km</Metric>
      <Metric>到手 ¥{yuan(o.fee.takehome_cents)}</Metric>
      {o.forced && <Metric>强制</Metric>}
      {o.back_order && <Metric>背单</Metric>}
      {o.urge_count ? <Metric>催 {o.urge_count}</Metric> : null}
    </MetricRow>
  );
}

/**
 * 时限状态。
 *
 * 关键状态交 `Badge` 而不是 `Tag`——规范里 Tag 只做分类标签，
 * 且 Tag 无法承载「点 + 文案」的状态语义。
 */
function ttlBadge(minutes: number, prefix: string): {
  status: "error" | "warning" | "default";
  text: string;
} {
  if (minutes < 0) return { status: "error", text: `${prefix}超时 ${-minutes} 分` };
  if (minutes < 10) return { status: "warning", text: `${prefix} ${minutes} 分` };
  return { status: "default", text: `${prefix} ${minutes} 分` };
}

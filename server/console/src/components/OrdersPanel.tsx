import { Card, Empty, Select, Space, Tag, Typography } from "antd";
import { STATUS_LABEL } from "../const";
import { useIsControl, useStore } from "../hooks";
import type { Order, RiderBrief } from "../protocol";
import { store, yuan } from "../store";

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
      <Section title="大厅待抢" count={snap.hall.length}>
        {snap.hall.length === 0 ? (
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            大厅是空的
          </Typography.Text>
        ) : (
          snap.hall.map((o) => (
            <HallCard
              key={o.id}
              order={o}
              elapsed={snap.elapsed_min}
              candidates={candidates}
              isControl={isControl}
            />
          ))
        )}
      </Section>

      <Section title="在跑" count={snap.active_orders.length}>
        {snap.active_orders.length === 0 ? (
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            没有在跑的单
          </Typography.Text>
        ) : (
          snap.active_orders.map((o) => (
            <ActiveCard key={o.id} order={o} elapsed={snap.elapsed_min} nameOf={nameOf} />
          ))
        )}
      </Section>
    </div>
  );
}

function Section({
  title,
  count,
  children,
}: {
  title: string;
  count: number;
  children: React.ReactNode;
}) {
  return (
    <div style={{ marginBottom: 12 }}>
      <Typography.Title level={5} style={{ marginTop: 0, marginBottom: 8 }}>
        {title} <Typography.Text type="secondary" style={{ fontSize: 12 }}>{count}</Typography.Text>
      </Typography.Title>
      <Space direction="vertical" size={8} style={{ width: "100%" }}>
        {children}
      </Space>
    </div>
  );
}

function HallCard({
  order: o,
  elapsed,
  candidates,
  isControl,
}: {
  order: Order;
  elapsed: number;
  candidates: RiderBrief[];
  isControl: boolean;
}) {
  const ttl = o.claim_deadline_min - elapsed;
  return (
    <Card size="small">
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <div style={{ flex: 1, minWidth: 0 }}>
          <b>#{o.id}</b> {o.merchant_name}{" "}
          <Typography.Text type="secondary">→ {o.customer_address}</Typography.Text>
        </div>
        <Typography.Text strong style={{ color: "#ff7a1a" }}>
          ¥{yuan(o.fee.quoted_fee_cents)}
        </Typography.Text>
      </div>
      <Typography.Text type="secondary" style={{ fontSize: 11 }}>
        {(o.distance_m / 1000).toFixed(1)}km · 承诺 {o.promised_minutes} 分 · 抽水 ¥
        {yuan(o.fee.commission_cents)}
        {o.task ? ` · 任务：${o.task.name}` : ""}
        {ttl >= 0 ? ` · 窗口剩 ${ttl} 分` : ""}
      </Typography.Text>
      {isControl && (
        <Space.Compact style={{ width: "100%", marginTop: 6 }}>
          <Select
            size="small"
            style={{ flex: 1 }}
            placeholder="选骑手"
            options={candidates.map((r) => ({
              value: r.id,
              label: `${r.name}（${r.held.length} 单）`,
            }))}
            onChange={(v) => store.send("force_assign", { order_id: o.id, rider_id: v })}
            value={null}
          />
        </Space.Compact>
      )}
    </Card>
  );
}

function ActiveCard({
  order: o,
  elapsed,
  nameOf,
}: {
  order: Order;
  elapsed: number;
  nameOf: (id: number | null) => string;
}) {
  const left = o.created_at_min + o.promised_minutes - elapsed;
  const ttlColor = left < 0 ? "red" : left < 10 ? "orange" : "default";
  return (
    <Card size="small">
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <div style={{ flex: 1, minWidth: 0 }}>
          <b>#{o.id}</b> {o.merchant_name}{" "}
          <Typography.Text type="secondary">→ {o.customer_address}</Typography.Text>
        </div>
        <Tag color={ttlColor} style={{ marginInlineEnd: 0 }}>
          {left < 0 ? `超时 ${-left} 分` : `剩 ${left} 分`}
        </Tag>
      </div>
      <Typography.Text type="secondary" style={{ fontSize: 11 }}>
        {STATUS_LABEL[o.status] ?? o.status} · 骑手 {nameOf(o.assigned_to)} ·{" "}
        {(o.distance_m / 1000).toFixed(1)}km · 到手 ¥{yuan(o.fee.takehome_cents)}
        {o.forced ? " · 强制" : ""}
        {o.back_order ? " · 背单" : ""}
        {o.urge_count ? ` · 催 ${o.urge_count}` : ""}
      </Typography.Text>
    </Card>
  );
}

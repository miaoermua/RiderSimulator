import { Badge, Empty, List, Space, Typography } from "antd";
import { STATUS_LABEL } from "../const";
import { useIsControl, useStore } from "../hooks";
import type { Order, RiderBrief } from "../protocol";
import { yuan } from "../store";
import { CreateOrderButton } from "./CreateOrderModal";
import { DispatchButton } from "./DispatchModal";
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

  const nameOf = (id: number | null) =>
    id == null ? "-" : (snap.riders.find((r) => r.id === id)?.name ?? `#${id}`);

  return (
    <div className="console-scroll">
      <List
        size="small"
        header={
          <Space style={{ width: "100%", justifyContent: "space-between" }}>
            <span>大厅待抢</span>
            {/* 临时插单放在列表头上：造出来的单立刻出现在下面，因果挨着。 */}
            <CreateOrderButton />
          </Space>
        }
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
                <>
                  <MetricRow>
                    <Metric>{(o.distance_m / 1000).toFixed(1)}km</Metric>
                    <Metric>承诺 {o.promised_minutes} 分</Metric>
                    <Metric>抽水 ¥{yuan(o.fee.commission_cents)}</Metric>
                    {o.redispatch_count > 0 && (
                      <Metric>
                        重投 {o.redispatch_count} 次 +¥{yuan(o.surcharge_cents)}
                      </Metric>
                    )}
                    {o.task && <Metric>任务 {o.task.name}</Metric>}
                  </MetricRow>
                  <MetricRow>
                    <Metric>{feeParts(o).join(" · ")}</Metric>
                  </MetricRow>
                </>
              }
            />
            {isControl && (
              <div style={{ marginTop: 8 }}>
                {/* 先选口径再选人：指派单可拒、强制派单只能同意，下发前就得说清。 */}
                <DispatchButton
                  order={o}
                  riders={snap.riders}
                  limit={snap.config.max_rejects_per_day}
                  kind="assign"
                  label="派单"
                />
              </div>
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
            {isControl && (
              <div style={{ marginTop: 8 }}>
                <ReassignAction
                  order={o}
                  riders={snap.riders}
                  limit={snap.config.max_rejects_per_day}
                  rider={nameOf(o.assigned_to)}
                />
              </div>
            )}
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
          {/* 报价 + 重投补贴才是骑手实际看到的价，标题行不能只印计费规则那一半。 */}
          <Typography.Text strong>¥{yuan(o.fee.quoted_fee_cents + o.surcharge_cents)}</Typography.Text>
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
      <Metric>到手 ¥{yuan(o.fee.takehome_cents + o.surcharge_cents)}</Metric>
      {o.redispatch_count > 0 && <Metric>重投 {o.redispatch_count} 次</Metric>}
      {/* 待确认时按口径区分；确认之后 `assign_mode` 就没了，只剩「平台派过」。 */}
      {o.assign_mode === "forced" && <Metric>强制派单</Metric>}
      {o.assign_mode === "offer" && <Metric>指派单</Metric>}
      {!o.assign_mode && o.forced && <Metric>平台派单</Metric>}
      {o.back_order && <Metric>背单</Metric>}
      {o.urge_count ? <Metric>催 {o.urge_count}</Metric> : null}
    </MetricRow>
  );
}

/**
 * 改派入口。
 *
 * 只在「已指派、还没取货」时出现：`assigned` 覆盖「去店路上」和「店里等餐」两段，
 * 这两段里餐还在商家手里，站长才有得调。一旦取到餐（`picked_up`），保温与餐损
 * 都随原骑手走，服务端会回 `WRONG_PHASE`——所以这里干脆不画，
 * 免得用户点出一个必然失败的动作。
 *
 * 改派和派单共用同一个弹窗：新骑手同样要选「指派单 / 强制派单」。
 */
function ReassignAction({
  order,
  riders,
  limit,
  rider,
}: {
  order: Order;
  riders: RiderBrief[];
  limit: number;
  rider: string;
}) {
  if (order.status !== "assigned" || order.assigned_to == null) return null;
  return (
    <DispatchButton
      order={order}
      riders={riders}
      limit={limit}
      kind="reassign"
      fromName={rider}
      label="改派"
    />
  );
}

/**
 * 把报价拆成能对账的几项。
 *
 * 用「·」而不是「+」连接：高峰是系数不是加项，混在一起会算错账。
 */
function feeParts(o: Order): string[] {
  const f = o.fee;
  const parts = [`起步 ¥${yuan(f.base_fee_cents)}`, `里程 ¥${yuan(f.distance_extra_cents)}`];
  if (f.level_bonus_cents > 0) parts.push(`等级 ¥${yuan(f.level_bonus_cents)}`);
  if (f.surge_multiplier !== 1) parts.push(`高峰 ×${f.surge_multiplier.toFixed(2)}`);
  if (f.rain_bonus_cents > 0) parts.push(`雨补 ¥${yuan(f.rain_bonus_cents)}`);
  if (o.surcharge_cents > 0) parts.push(`重投补贴 ¥${yuan(o.surcharge_cents)}`);
  return parts;
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

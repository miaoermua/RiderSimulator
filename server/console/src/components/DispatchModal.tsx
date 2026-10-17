import { useState } from "react";
import { Alert, App as AntApp, Button, Modal, Radio, Select, Space, Typography } from "antd";
import type { AssignMode, Order, RiderBrief } from "../protocol";
import { store, yuan } from "../store";

/**
 * 「派单」入口：大厅塞单 / 在跑改派共用。
 *
 * 改造前是一个「指派给…」下拉，选完立刻下发，骑手**总能拒**。现在下发之前
 * 先让站长选口径：
 *
 * - 指派单（`offer`）：骑手端弹确认，可以拒绝，拒一次吃一格当日额度；
 * - 强制派单（`forced`）：骑手端只留「同意」，30 秒不点自动接下。
 *
 * 选择只在打开时重置，快照刷新不会把选到一半的对象冲掉（本面板订阅的是
 * 快照而不是 tick）。
 */
export function DispatchButton({
  order,
  riders,
  limit,
  kind,
  fromName,
  label,
}: {
  order: Order;
  riders: RiderBrief[];
  /** 当日拒单上限，`0` = 不限；只用于把「拒一次」的代价说清楚。 */
  limit: number;
  kind: "assign" | "reassign";
  /** 改派时的原骑手名字，给他一句「还没取餐」的上下文。 */
  fromName?: string;
  label: string;
}) {
  const { message } = AntApp.useApp();
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<AssignMode>("offer");
  const [riderId, setRiderId] = useState<number | null>(null);

  const openDialog = () => {
    setMode("offer");
    setRiderId(null);
    setOpen(true);
  };

  const candidates = riders.filter(
    (r) =>
      !r.injured &&
      !r.frozen &&
      // 被强制下线的人不能自己上线，但平台仍能给他塞强制单——只有这一种。
      (r.online || r.forced_offline) &&
      !(kind === "reassign" && r.id === order.assigned_to),
  );
  const picked = candidates.find((r) => r.id === riderId) ?? null;
  // 服务端也会纠正（`forced_offline` 的人只能收强制单），这里提前说破，
  // 免得站长以为自己下发的是「可拒」的指派单。
  const upgradeToForced = picked?.forced_offline === true && mode === "offer";

  const submit = () => {
    if (riderId == null) {
      message.error("先选一个骑手");
      return;
    }
    if (kind === "assign") {
      store.send("force_assign", {
        order_id: order.id,
        rider_id: riderId,
        mode: upgradeToForced ? "forced" : mode,
      });
    } else {
      store.reassign(order.id, riderId, upgradeToForced ? "forced" : mode);
    }
    setOpen(false);
  };

  return (
    <>
      <Button onClick={openDialog}>{label}</Button>
      <Modal
        open={open}
        title={kind === "assign" ? `派单 #${order.id}` : `改派 #${order.id}`}
        width={480}
        okText="下发"
        cancelText="取消"
        onOk={submit}
        onCancel={() => setOpen(false)}
        destroyOnClose
      >
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Typography.Text type="secondary">
            {kind === "assign"
              ? `${order.merchant_name} → ${order.customer_address}，报价 ¥${yuan(
                  order.fee.quoted_fee_cents + order.surcharge_cents,
                )}`
              : `${order.merchant_name} → ${order.customer_address}；原骑手 ${
                  fromName ?? "-"
                } 还没取到餐`}
          </Typography.Text>

          <div>
            <Typography.Text strong>派单方式</Typography.Text>
            <Radio.Group
              style={{ display: "flex", marginTop: 6 }}
              value={mode}
              onChange={(e) => setMode(e.target.value as AssignMode)}
            >
              <Radio value="offer">
                指派单
                <Typography.Text type="secondary" style={{ fontSize: 12, marginInlineStart: 6 }}>
                  {limit === 0 ? "可拒，不限次数" : `可拒，占用当日 ${limit} 次额度`}
                </Typography.Text>
              </Radio>
              <Radio value="forced">
                强制派单
                <Typography.Text type="secondary" style={{ fontSize: 12, marginInlineStart: 6 }}>
                  只能同意，30 秒自动接下
                </Typography.Text>
              </Radio>
            </Radio.Group>
          </div>

          <div>
            <Typography.Text strong>派给</Typography.Text>
            <Select
              style={{ width: "100%", marginTop: 6 }}
              showSearch
              optionFilterProp="label"
              placeholder="选一个骑手"
              value={riderId}
              onChange={(v: number) => setRiderId(v)}
              options={candidates.map((r) => ({
                value: r.id,
                label:
                  `${r.name}（${r.held.length} 单 · 今日拒 ${r.rejects_today}` +
                  `${limit > 0 ? `/${limit}` : ""}）` +
                  (r.forced_offline ? " · 已强制下线" : ""),
              }))}
            />
          </div>

          {upgradeToForced && (
            <Alert
              type="warning"
              showIcon
              message="这个骑手已被强制下线"
              description="他今天不能再自己上线，只能接强制单；这一单会按「强制派单」下发。"
            />
          )}
        </Space>
      </Modal>
    </>
  );
}

import { useState } from "react";
import { App as AntApp, Button, Card, Empty, Input, Modal, Space, Tag, Typography } from "antd";
import { PHASE_LABEL, SPEED_MODE_LABEL } from "../const";
import { useIsControl, useStore } from "../hooks";
import type { RiderBrief } from "../protocol";
import { store, yuan } from "../store";

interface Props {
  selected: number | null;
  onSelect: (id: number) => void;
}

export function RidersPanel({ selected, onSelect }: Props) {
  const snap = useStore((s) => s.snap);
  const isControl = useIsControl();
  const { message } = AntApp.useApp();
  const [renaming, setRenaming] = useState<RiderBrief | null>(null);
  const [draft, setDraft] = useState("");

  if (!snap) return <Empty style={{ marginTop: 48 }} description="等待快照…" />;

  const riders = [...snap.riders].sort((a, b) => a.id - b.id);

  const openRename = (r: RiderBrief) => {
    setRenaming(r);
    setDraft(r.name);
  };

  const submitRename = () => {
    if (!renaming) return;
    const name = draft.trim();
    if (!name) {
      message.warning("名字不能为空");
      return;
    }
    store.send("rename_rider", { rider_id: renaming.id, name });
    setRenaming(null);
  };

  const st = snap.stats;
  const online = snap.riders.filter((r) => r.online).length;
  const busy = snap.riders.filter((r) => r.held.length > 0).length;
  const stats: Array<[string | number, string]> = [
    [`${online}/${snap.riders.length}`, "在线骑手"],
    [busy, "配送中"],
    [snap.hall.length, "大厅待抢"],
    [snap.active_orders.length, "在跑订单"],
    [st.orders_created, "累计生成"],
    [st.orders_delivered, "已送达"],
    [st.orders_late, "超时"],
    [st.orders_expired, "流单"],
  ];

  return (
    <div className="console-scroll">
      <div className="stat-grid">
        {stats.map(([v, k]) => (
          <div key={k} className="stat-cell">
            <b>{v}</b>
            <span>{k}</span>
          </div>
        ))}
      </div>

      <Space direction="vertical" size={8} style={{ width: "100%" }}>
        {riders.map((r) => (
          <Card
            key={r.id}
            size="small"
            hoverable
            onClick={() => onSelect(r.id)}
            style={{
              borderColor: selected === r.id ? "#ff7a1a" : undefined,
              opacity: r.online ? 1 : 0.6,
            }}
            title={
              <Space size={6}>
                <span style={{ color: r.kind === "human" ? "#9b59b6" : "#ff7a1a" }}>●</span>
                <span>{r.name}</span>
                <Typography.Text type="secondary" style={{ fontSize: 12 }}>#{r.id}</Typography.Text>
                {r.kind === "human" && <Tag color="purple" style={{ marginInlineEnd: 0 }}>真人</Tag>}
                {!r.online && <Tag style={{ marginInlineEnd: 0 }}>离线</Tag>}
                {r.frozen && <Tag color="blue" style={{ marginInlineEnd: 0 }}>冻结</Tag>}
                {r.injured && <Tag color="red" style={{ marginInlineEnd: 0 }}>受伤</Tag>}
              </Space>
            }
            extra={
              isControl && (
                <Space size={4}>
                  <Button
                    size="small"
                    onClick={(e) => {
                      e.stopPropagation();
                      openRename(r);
                    }}
                  >
                    改名
                  </Button>
                  <Button
                    size="small"
                    type={r.online ? "default" : "primary"}
                    onClick={(e) => {
                      e.stopPropagation();
                      store.send("set_rider_online", { rider_id: r.id, online: !r.online });
                    }}
                  >
                    {r.online ? "下线" : "上线"}
                  </Button>
                </Space>
              )
            }
          >
            <Space size={12} wrap style={{ fontSize: 12 }}>
              <span>Lv{r.level}</span>
              <span>评分 {r.rating.toFixed(2)}</span>
              <span>完成 {r.completed}</span>
              <span>
                持单 {r.held.length > 0 ? `#${r.held.join(" #")}` : "0"}
              </span>
              <span>{r.phase ? PHASE_LABEL[r.phase] : "空闲"}</span>
              <span>{SPEED_MODE_LABEL[r.speed_mode] ?? r.speed_mode}</span>
              {r.hourly_wage_cents != null && <span>时薪 ¥{yuan(r.hourly_wage_cents)}</span>}
              {r.takehome_cents != null && <span>到手 ¥{yuan(r.takehome_cents)}</span>}
              {r.fatigue != null && <span>疲劳 {Math.round(r.fatigue * 100)}%</span>}
            </Space>
          </Card>
        ))}
      </Space>

      <Modal
        open={renaming !== null}
        title={`给 ${renaming?.name ?? ""} 改名`}
        okText="保存"
        cancelText="取消"
        onOk={submitRename}
        onCancel={() => setRenaming(null)}
      >
        <Input
          value={draft}
          maxLength={12}
          showCount
          placeholder="最多 12 字"
          onChange={(e) => setDraft(e.target.value)}
          onPressEnter={submitRename}
        />
      </Modal>
    </div>
  );
}

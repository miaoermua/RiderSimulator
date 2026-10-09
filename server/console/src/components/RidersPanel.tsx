import { useState } from "react";
import {
  App as AntApp,
  Badge,
  Button,
  Card,
  Empty,
  Input,
  List,
  Modal,
  Space,
  Statistic,
  Tag,
  Typography,
  theme,
} from "antd";
import { HUMAN_RIDER_COLOR, PHASE_LABEL, SPEED_MODE_LABEL } from "../const";
import { useIsControl, useStore } from "../hooks";
import type { RiderBrief } from "../protocol";
import { store, yuan } from "../store";
import { Metric, MetricRow } from "./Metric";

interface Props {
  selected: number | null;
  onSelect: (id: number) => void;
}

export function RidersPanel({ selected, onSelect }: Props) {
  const snap = useStore((s) => s.snap);
  const isControl = useIsControl();
  const { token } = theme.useToken();
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
      {/* 统计是「读」的信息：标题用次级文本，数值用标题级字号并锁表格数字。 */}
      <Card size="small" style={{ marginBottom: 16 }}>
        <div className="stat-grid">
          {stats.map(([v, k]) => (
            <Statistic
              key={k}
              title={k}
              value={v}
              valueStyle={{
                fontSize: token.fontSizeHeading4,
                fontVariantNumeric: "tabular-nums",
              }}
            />
          ))}
        </div>
      </Card>

      <List
        size="small"
        header="骑手"
        itemLayout="vertical"
        rowKey="id"
        dataSource={riders}
        renderItem={(r) => (
          <List.Item
            onClick={() => onSelect(r.id)}
            style={{
              paddingInline: 8,
              borderRadius: token.borderRadius,
              cursor: "pointer",
              // 选中态取 antd 的「导航里你在这儿」那一档，不另造颜色。
              background: selected === r.id ? token.controlItemBgActive : undefined,
            }}
            actions={
              isControl
                ? [
                    <Button
                      key="rename"
                      size="small"
                      onClick={(e) => {
                        e.stopPropagation();
                        openRename(r);
                      }}
                    >
                      改名
                    </Button>,
                    <Button
                      key="online"
                      size="small"
                      onClick={(e) => {
                        e.stopPropagation();
                        store.send("set_rider_online", { rider_id: r.id, online: !r.online });
                      }}
                    >
                      {r.online ? "下线" : "上线"}
                    </Button>,
                  ]
                : undefined
            }
          >
            <List.Item.Meta
              title={
                <Space size={8}>
                  {/* 在线与否是关键状态，用 Badge 的点，不用彩色正文。 */}
                  <Badge status={r.online ? "success" : "default"} />
                  <Typography.Text
                    strong
                    style={{ color: r.kind === "human" ? HUMAN_RIDER_COLOR : undefined }}
                  >
                    {r.name}
                  </Typography.Text>
                  <Metric>#{r.id}</Metric>
                  {r.kind === "human" && <Tag color="purple">真人</Tag>}
                  {r.frozen && <Badge status="warning" text="冻结" />}
                  {r.injured && <Badge status="error" text="受伤" />}
                </Space>
              }
              description={
                <MetricRow>
                  <Metric>Lv{r.level}</Metric>
                  <Metric>评分 {r.rating.toFixed(2)}</Metric>
                  <Metric>完成 {r.completed}</Metric>
                  <Metric>持单 {r.held.length > 0 ? `#${r.held.join(" #")}` : "0"}</Metric>
                  <Metric>{r.phase ? PHASE_LABEL[r.phase] : "空闲"}</Metric>
                  <Metric>{SPEED_MODE_LABEL[r.speed_mode] ?? r.speed_mode}</Metric>
                  {r.hourly_wage_cents != null && <Metric>时薪 ¥{yuan(r.hourly_wage_cents)}</Metric>}
                  {r.takehome_cents != null && <Metric>到手 ¥{yuan(r.takehome_cents)}</Metric>}
                  {r.fatigue != null && <Metric>疲劳 {Math.round(r.fatigue * 100)}%</Metric>}
                </MetricRow>
              }
            />
          </List.Item>
        )}
      />

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

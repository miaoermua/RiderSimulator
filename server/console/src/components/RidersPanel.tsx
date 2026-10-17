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
  const { message, modal } = AntApp.useApp();
  const [renaming, setRenaming] = useState<RiderBrief | null>(null);
  const [draft, setDraft] = useState("");

  if (!snap) return <Empty style={{ marginTop: 48 }} description="等待快照…" />;

  const riders = [...snap.riders].sort((a, b) => a.id - b.id);
  const rejectLimit = snap.config.max_rejects_per_day;

  const openRename = (r: RiderBrief) => {
    setRenaming(r);
    setDraft(r.name);
  };

  /**
   * 清空钱包。**两次确认**：第一次表明意图，第二次把「现在是多少钱」摆到眼前。
   *
   * 这个动作用来做测试降噪（把账玩乱了就清零重来），本身没有任何撤销入口，
   * 所以第二次确认必须带上具体金额——不然连点两下就把真数据抹了。
   */
  const confirmClearWallet = (r: RiderBrief) => {
    modal.confirm({
      title: `清空 ${r.name} 的钱包？`,
      content: "只是把账清零，方便继续测试。流水与罚单也会一并抹掉，不可撤销。",
      okText: "继续",
      cancelText: "取消",
      onOk: () => {
        modal.confirm({
          title: "再确认一次",
          content: `当前余额 ¥${yuan(r.cash_cents ?? 0)}，清空后归零。`,
          okText: "确认清空",
          okButtonProps: { danger: true },
          cancelText: "我再想想",
          onOk: () => store.resetRiderWallet(r.id),
        });
      },
    });
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
                      // 被强制下线的人当天不可再上线，服务端也会挡；按钮直接禁掉。
                      disabled={r.forced_offline && !r.online}
                      onClick={(e) => {
                        e.stopPropagation();
                        store.send("set_rider_online", { rider_id: r.id, online: !r.online });
                      }}
                    >
                      {r.forced_offline && !r.online ? "今日禁上线" : r.online ? "下线" : "上线"}
                    </Button>,
                    <Button
                      key="wallet"
                      size="small"
                      danger
                      onClick={(e) => {
                        e.stopPropagation();
                        confirmClearWallet(r);
                      }}
                    >
                      清空钱包
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
                  {/* 拒单额度用尽 → 当天被强制下线，次日日结才解封。 */}
                  {r.forced_offline && <Badge status="error" text="强制下线" />}
                </Space>
              }
              description={
                <MetricRow>
                  <Metric>Lv{r.level}</Metric>
                  <Metric>评分 {r.rating.toFixed(2)}</Metric>
                  <Metric>完成 {r.completed}</Metric>
                  <Metric>持单 {r.held.length > 0 ? `#${r.held.join(" #")}` : "0"}</Metric>
                  {/* 拒单额度是当天的一次性资源，用掉多少得一眼看得见。 */}
                  <Metric>
                    拒单 {r.rejects_today}
                    {rejectLimit > 0 ? `/${rejectLimit}` : "（不限）"}
                  </Metric>
                  <Metric>{r.phase ? PHASE_LABEL[r.phase] : "空闲"}</Metric>
                  {/* 「持单」只说明手上有哪几张单，先后顺序要看路线。
                      一腿的没什么可看的（phase 已经说了他在干嘛），两腿以上才渲染。 */}
                  {r.route.length > 1 && (
                    <Metric>
                      路线{" "}
                      {r.route.map((leg, i) => (
                        <span key={`${leg.order_id}-${leg.kind}`}>
                          {i > 0 && " → "}
                          {i + 1}.{leg.kind === "pickup" ? "取" : "送"}#{leg.order_id}
                        </span>
                      ))}
                      （本腿还剩 {r.route[0].remaining_min} 分）
                    </Metric>
                  )}
                  <Metric>{SPEED_MODE_LABEL[r.speed_mode] ?? r.speed_mode}</Metric>
                  <Wallet cents={r.cash_cents} />
                  {r.hourly_wage_cents != null && <Metric>时薪 ¥{yuan(r.hourly_wage_cents)}</Metric>}
                  {r.takehome_cents != null && <Metric>到手 ¥{yuan(r.takehome_cents)}</Metric>}
                  {r.fatigue != null && <Metric>疲劳 {Math.round(r.fatigue * 100)}%</Metric>}
                  {/* 情绪低的时候直接标「摆烂」：光给个百分比看不出后果，
                      得让人一眼明白「他为什么磨蹭、为什么挑单」。 */}
                  {r.mood != null && (
                    <Metric>
                      情绪 {Math.round(r.mood * 100)}%{r.slacking && "（摆烂）"}
                    </Metric>
                  )}
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

/**
 * 钱包余额。
 *
 * **允许为负**（罚单超过当日收入时骑手就是倒欠平台），负数是真正的关键状态，
 * 所以取语义色标出来，而不是和别的元信息一样灰着。
 */
function Wallet({ cents }: { cents: number | null }) {
  if (cents == null) return null;
  if (cents < 0) {
    return (
      <Typography.Text type="danger" style={{ fontSize: 12 }}>
        钱包 -¥{yuan(-cents)}
      </Typography.Text>
    );
  }
  return <Metric>钱包 ¥{yuan(cents)}</Metric>;
}

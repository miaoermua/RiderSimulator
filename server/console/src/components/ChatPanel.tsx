import { Card, Empty, Segmented, Space, Tag, Typography } from "antd";
import { useState } from "react";
import { useStore } from "../hooks";
import { FAULT_LABEL, yuan, type ChatEntry } from "../store";

/** 说话人。`from` 是协议里的机器名，界面上不该出现。 */
const FROM_LABEL: Record<string, string> = {
  rider: "骑手",
  customer: "顾客",
  system: "系统",
};

const FROM_COLOR: Record<string, string> = {
  rider: "blue",
  customer: "gold",
  system: "default",
};

/** 消息类型。骑手「照着念的」和「自己打的」要分开——那是审计关心的差别。 */
const KIND_LABEL: Record<string, string> = {
  preset: "预设",
  free: "手输",
  reply: "回应",
  ruling: "裁决",
};

/**
 * 订单会话（末端交付沟通）。
 *
 * 与日志分开的核心理由：日志是**时间序**的流水，会话要**按单聚合**。
 * 烽火台要看的是「这一单骑手和顾客来回说了什么、最后赔了多少钱」，
 * 所以这里按订单分组、组内按时间正序读，组与组之间新会话在前。
 *
 * 时效只有一天：服务端日结时清库，这边跟着 `day_closed` 清空，
 * 面板上把这条规则显式写出来，免得有人当成历史记录翻。
 */
export function ChatPanel() {
  const chats = useStore((s) => s.chats);
  /** 只订阅天号这个基本类型：快照每秒来一次，天号不变就不重渲染。 */
  const day = useStore((s) => s.snap?.day ?? null);
  const [scope, setScope] = useState<"all" | "costly">("all");

  const costly = chats.filter((c) => c.compensationCents > 0);
  const groups = groupByOrder(scope === "costly" ? costly : chats);

  return (
    <div className="console-scroll">
      <Space
        style={{ width: "100%", justifyContent: "space-between", marginBottom: 12 }}
        wrap
      >
        <Typography.Text type="secondary">
          {day === null ? "" : `第 ${day} 天 · `}
          会话只保留一天，日结时连同库里一起清掉。
        </Typography.Text>
        <Segmented
          size="small"
          value={scope}
          onChange={(v) => setScope(v as typeof scope)}
          options={[
            { label: `全部 ${chats.length}`, value: "all" },
            { label: `有赔付 ${costly.length}`, value: "costly" },
          ]}
        />
      </Space>

      {groups.length === 0 ? (
        <Empty
          style={{ marginTop: 48 }}
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description={
            chats.length === 0 ? "还没有订单会话" : "当前筛选下没有会话"
          }
        />
      ) : (
        groups.map((g) => <OrderChat key={g.orderId} group={g} />)
      )}
    </div>
  );
}

interface ChatGroup {
  orderId: number;
  /** 组内**按时间正序**（和读聊天记录一样）。 */
  lines: ChatEntry[];
}

/**
 * 按订单分组。
 *
 * `chats` 是新的在前，所以先出现的订单就是最近有动静的那一单——
 * `Map` 的插入序正好就是组间要的顺序，不必再排一次。
 */
function groupByOrder(chats: ChatEntry[]): ChatGroup[] {
  const map = new Map<number, ChatEntry[]>();
  for (const c of chats) {
    const list = map.get(c.orderId);
    if (list) list.push(c);
    else map.set(c.orderId, [c]);
  }
  return [...map.entries()].map(([orderId, lines]) => ({
    orderId,
    lines: [...lines].reverse(),
  }));
}

/** 一单的会话。标题给单号与条数，右上角优先亮出这一单总共赔了多少。 */
function OrderChat({ group }: { group: ChatGroup }) {
  const cost = group.lines.reduce((s, c) => s + c.compensationCents, 0);

  return (
    <Card
      size="small"
      style={{ marginBottom: 12 }}
      title={
        <Space size={8}>
          <Typography.Text strong>#{group.orderId}</Typography.Text>
          <Typography.Text type="secondary">{group.lines.length} 条</Typography.Text>
        </Space>
      }
      extra={
        cost > 0 ? (
          <Typography.Text type="danger">赔付 ¥{yuan(cost)}</Typography.Text>
        ) : (
          <Typography.Text type="secondary">无赔付</Typography.Text>
        )
      }
    >
      {group.lines.map((c) => (
        <ChatLineRow key={c.id} line={c} />
      ))}
    </Card>
  );
}

function ChatLineRow({ line: c }: { line: ChatEntry }) {
  // 顾客那边说话人已经写在标签上了，再打一个「回应」是噪音；
  // 骑手的「预设 / 手输」和裁决结论才需要额外标注。
  const kind = c.from === "rider" || c.kind === "ruling" ? KIND_LABEL[c.kind] : undefined;

  return (
    <div style={{ marginBottom: 8 }}>
      <Space size={8} align="baseline" wrap>
        <Typography.Text type="secondary" className="log-time">
          {c.at}
        </Typography.Text>
        <Tag color={FROM_COLOR[c.from] ?? "default"} style={{ marginInlineEnd: 0 }}>
          {FROM_LABEL[c.from] ?? c.from}
        </Tag>
        {kind && <Tag style={{ marginInlineEnd: 0 }}>{kind}</Tag>}
        <Typography.Text>{c.text}</Typography.Text>
      </Space>

      {/* 赔付单独一行：它是这条消息的**后果**，跟正文混在一起会被读漏。 */}
      {c.compensationCents > 0 && (
        <div style={{ marginTop: 4 }}>
          <Typography.Text type="danger">
            赔付 ¥{yuan(c.compensationCents)} · 钱从
            {FAULT_LABEL[c.fault] ?? c.fault}身上出
          </Typography.Text>
        </div>
      )}
    </div>
  );
}

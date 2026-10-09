import { Empty, List, Space, Tag, Typography } from "antd";
import { useStore } from "../hooks";

/**
 * 事件日志。
 *
 * 条目是结构化数据（原来靠 `innerHTML` 拼字符串，商家名里带 `<` 就会破版），
 * 渲染交给 React 转义。列表用 antd 的 `List`，分隔线与内边距都来自令牌。
 */
export function LogPanel() {
  const logs = useStore((s) => s.logs);

  if (logs.length === 0) {
    return <Empty style={{ marginTop: 48 }} description="还没有事件" />;
  }

  return (
    <div className="console-scroll">
      <List
        size="small"
        rowKey="id"
        dataSource={logs}
        renderItem={(l) => (
          <List.Item style={{ display: "block", paddingInline: 0 }}>
            <Space size={8} align="baseline">
              <Typography.Text type="secondary" className="log-time">
                {l.at}
              </Typography.Text>
              {l.tag && (
                <Tag color={l.tagColor} style={{ marginInlineEnd: 0 }}>
                  {l.tag}
                </Tag>
              )}
              <Typography.Text>{l.text}</Typography.Text>
            </Space>
          </List.Item>
        )}
      />
    </div>
  );
}

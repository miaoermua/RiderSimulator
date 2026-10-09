import { Empty, Tag, Typography } from "antd";
import { useStore } from "../hooks";

/**
 * 事件日志。
 *
 * 原来靠 `innerHTML` 拼字符串，现在改成结构化条目数组——
 * 文本不再经过 HTML 解析，服务端来的商家名里带 `<` 也不会破版。
 */
export function LogPanel() {
  const logs = useStore((s) => s.logs);

  if (logs.length === 0) {
    return <Empty style={{ marginTop: 48 }} description="还没有事件" />;
  }

  return (
    <div className="console-scroll">
      {logs.map((l) => (
        <div key={l.id} className="log-item">
          <Typography.Text type="secondary" className="log-time">
            {l.at}
          </Typography.Text>
          {l.tag && (
            <Tag color={l.tagColor} style={{ marginInlineEnd: 6 }}>
              {l.tag}
            </Tag>
          )}
          <span>{l.text}</span>
        </div>
      ))}
    </div>
  );
}

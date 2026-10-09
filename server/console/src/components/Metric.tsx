import { Space, Typography, theme } from "antd";

/**
 * 密集元信息。
 *
 * 命中规范里的 `body-sm`（正文小号）与次级文本色——列表项里塞十几个字段时，
 * 只有降一档字号才不会把「标题 / 正文 / 元信息」三层压成一层。
 */
export function Metric({ children }: { children: React.ReactNode }) {
  const { token } = theme.useToken();
  return (
    <Typography.Text type="secondary" style={{ fontSize: token.fontSizeSM }}>
      {children}
    </Typography.Text>
  );
}

/** 一行元信息。间距取 md 档 16px，自动换行。 */
export function MetricRow({ children }: { children: React.ReactNode }) {
  return (
    <Space size={16} wrap>
      {children}
    </Space>
  );
}

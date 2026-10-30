import type { ReactNode } from "react";
import { Typography } from "antd";

/**
 * 面板内的分组标题。
 *
 * 原本 `AccessPanel` 和 `SettingsPanel` 各自写了一个 `Heading`，实现一模一样
 * 却分居两个文件——各自演化就会不一致（一处改字号、另一处忘改，两个面板
 * 的标题就悄悄错开一档）。抽到这里后，"面板标题长什么样"只有一个答案。
 *
 * 层级固定 `level={5}`：与 `Card` 的 title 同档，保证面板内"卡片标题"和
 * "分组标题"不会互相压过。要更强的层级用页面级标题，不要在这里升。
 *
 * `inline` 给「标题 + 右侧按钮」这一行用（例如「账号名单 [刷新]」）。
 * 那种场景下标题不能带上下外边距，否则会把同行的按钮顶歪。
 */
export function PanelHeading({ children, inline }: { children: ReactNode; inline?: boolean }) {
  return (
    <Typography.Title
      level={5}
      style={inline ? { margin: 0 } : { marginTop: 24, marginBottom: 8 }}
    >
      {children}
    </Typography.Title>
  );
}

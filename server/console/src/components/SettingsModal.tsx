import { useState, type ReactNode } from "react";
import { Menu, Modal, theme, type MenuProps } from "antd";
import {
  GiftOutlined,
  RobotOutlined,
  SettingOutlined,
  SlidersOutlined,
  UsergroupAddOutlined,
} from "@ant-design/icons";
import { AccessPanel } from "./AccessPanel";
import { ActivitiesPanel } from "./ActivitiesPanel";
import { AiPanel } from "./AiPanel";
import { SettingsPanel } from "./SettingsPanel";

/**
 * 控制台设置弹窗：左侧分区菜单，右侧主内容。
 *
 * ## 为什么从侧栏标签搬到这里
 *
 * 侧栏那排标签原本有 8 个（骑手 / 订单 / 会话 / AI / 骑手接入 / 活动 / 设置 / 日志），
 * 420px 宽根本放不下，只能横向滚动、还折字。更要紧的是这 8 个混了两类东西：
 *
 * - **跑单时要盯的**：骑手、订单、会话、日志——一直在看；
 * - **偶尔才改的**：AI 开关、接入账号、活动上下架、运行参数——配置一次就不动了。
 *
 * 后者占了一半标签宽度，却几乎不点。收进弹窗后侧栏只剩前四个，
 * 横向空间够了，也不用再滚动。
 *
 * ## 布局
 *
 * 左 208px 菜单 + 右主内容，中间一条 1px 边框分割，**不用阴影**——
 * 与页头/侧栏的三层表面模型保持一致（容器面靠弱边框分层，不靠 elevation）。
 *
 * ## 挂载策略
 *
 * 只渲染当前选中的那一个面板，切走就卸载。有意的：
 * `AiPanel` 挂载时会 `queryAi()`、`AccessPanel` 会拉账号列表，
 * 切一次刷新一次反而是想要的行为。代价是切换瞬间有一次 loading，
 * 相对"打开弹窗看到十分钟前的决策流"更划算。
 */
type Section = "ai" | "access" | "activities" | "runtime";

const MENU: MenuProps["items"] = [
  {
    type: "group",
    label: "运营",
    children: [
      { key: "ai", icon: <RobotOutlined />, label: "AI 决策" },
      { key: "access", icon: <UsergroupAddOutlined />, label: "骑手接入" },
      { key: "activities", icon: <GiftOutlined />, label: "平台活动" },
    ],
  },
  {
    type: "group",
    label: "系统",
    children: [{ key: "runtime", icon: <SlidersOutlined />, label: "运行设置" }],
  },
];

/** 侧栏宽度。菜单只有四项，固定值比自适应更稳，也不会随文案抖动。 */
const NAV_W = 200;

export function SettingsModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { token } = theme.useToken();
  const [section, setSection] = useState<Section>("ai");

  const panels: Record<Section, ReactNode> = {
    ai: <AiPanel />,
    access: <AccessPanel />,
    activities: <ActivitiesPanel />,
    runtime: <SettingsPanel />,
  };

  return (
    <Modal
      open={open}
      onCancel={onClose}
      footer={null}
      centered
      width={1040}
      // 窄屏（笔记本小窗、平板）时别顶出视口。antd 的 width 只认数字，
      // 所以上限交给 maxWidth——`min()` 写在 width 上不会被它解析。
      style={{ maxWidth: "94vw" }}
      title={
        <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
          <SettingOutlined />
          控制台设置
        </span>
      }
      // body 归零内边距让左右两栏自己贴边；高度固定，否则切到长面板时
      // 弹窗会一会儿高一会儿矮，视线要跟着跳。
      styles={{
        body: {
          padding: 0,
          display: "flex",
          height: "min(660px, 76vh)",
          overflow: "hidden",
        },
      }}
    >
      <nav
        style={{
          width: NAV_W,
          flex: `0 0 ${NAV_W}px`,
          overflow: "auto",
          padding: 8,
          // 侧栏是「底板」而不是「内容面」：底色比右侧淡一档，
          // 这样视觉上是一条沟，不是两块并列的卡片。
          background: token.colorBgLayout,
          borderInlineEnd: `1px solid ${token.colorBorderSecondary}`,
        }}
      >
        <Menu
          mode="inline"
          items={MENU}
          selectedKeys={[section]}
          onClick={({ key }) => setSection(key as Section)}
          style={{
            // 菜单本身透明，让底色由上面的 nav 统一决定，
            // 否则 antd 会给 Menu 再刷一层白，沟就没了。
            background: "transparent",
            borderInlineEnd: "none",
          }}
        />
      </nav>

      {/* 主内容。四个面板自己都带 `.console-scroll`（height:100% + 16px 内边距
          + overflow:auto），所以这里只负责给足宽度，不要再套 padding。 */}
      <div style={{ flex: 1, minWidth: 0, background: token.colorBgContainer }}>
        {panels[section]}
      </div>
    </Modal>
  );
}

import { useCallback, useEffect, useRef, useState } from "react";
import { App as AntApp, Layout, Tabs, theme } from "antd";
import { useStore } from "./hooks";
import { ChatPanel } from "./components/ChatPanel";
import { LoginGate } from "./components/LoginGate";
import { LogPanel } from "./components/LogPanel";
import { MapView } from "./components/MapView";
import { OrdersPanel } from "./components/OrdersPanel";
import { RidersPanel } from "./components/RidersPanel";
import { SettingsModal } from "./components/SettingsModal";
import { TopBar } from "./components/TopBar";
import { store } from "./store";

/** 侧栏宽度：下界保证标签还能看，上界给地图留够地方。 */
const SIDER_MIN = 380;
const SIDER_MAX = 720;
const SIDER_DEFAULT = 420;
/** 4 个标签按中文 14px 实测约 320px，380px 的下界已经放得下，不需要再滚动。 */

export function App() {
  const { message } = AntApp.useApp();
  const { token } = theme.useToken();
  const role = useStore((s) => s.role);
  const [tab, setTab] = useState("riders");
  const [selected, setSelected] = useState<number | null>(null);
  const [sider, setSider] = useState(SIDER_DEFAULT);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const dragging = useRef(false);

  // store 不直接依赖 antd，提示能力在挂载时注入。
  useEffect(() => {
    store.setNotifier((kind, text) => message[kind](text));
  }, [message]);

  // 拖拽改侧栏宽度。用 pointer 事件而非 mouse：触控板与触屏也能用。
  const onResizeMove = useCallback((e: PointerEvent) => {
    if (!dragging.current) return;
    const next = window.innerWidth - e.clientX;
    setSider(Math.min(SIDER_MAX, Math.max(SIDER_MIN, next)));
  }, []);
  const stopResize = useCallback(() => {
    dragging.current = false;
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
  }, []);
  useEffect(() => {
    window.addEventListener("pointermove", onResizeMove);
    window.addEventListener("pointerup", stopResize);
    // 拖拽途中可能被切走标签/切窗口，卸载时务必复位光标，否则页面一直显示 col-resize。
    return () => {
      window.removeEventListener("pointermove", onResizeMove);
      window.removeEventListener("pointerup", stopResize);
      stopResize();
    };
  }, [onResizeMove, stopResize]);

  const startResize = useCallback((e: React.PointerEvent) => {
    e.preventDefault();
    dragging.current = true;
    document.body.style.cursor = "col-resize";
    // 选中文字会让拖拽出现蓝块闪烁。
    document.body.style.userSelect = "none";
  }, []);

  const pickRider = (id: number) => {
    setSelected((cur) => (cur === id ? null : id));
    setTab("riders");
  };

  // 只留「跑单时要盯的」四个。AI / 骑手接入 / 活动 / 设置已经收进右上角的
  // 设置弹窗——那四样配一次就不动了，占着标签宽度却几乎不点。
  const panels = [
    { key: "riders", label: "骑手", children: <RidersPanel selected={selected} onSelect={pickRider} /> },
    { key: "orders", label: "订单", children: <OrdersPanel /> },
    { key: "chat", label: "会话", children: <ChatPanel /> },
    { key: "log", label: "日志", children: <LogPanel /> },
  ];

  return (
    <Layout style={{ height: "100vh" }}>
      {/* 页头是容器面（白），与页面底板（#F5F5F5）靠一条弱边框分层，不用阴影。
          高度固定成 56px：顶栏内部所有控件已统一到 32 高，这里只需保证外框
          不会被内容撑开，否则某一项字号变化就会让整条页头跟着抖。 */}
      <Layout.Header
        style={{
          display: "flex",
          alignItems: "center",
          gap: 16,
          height: 56,
          padding: "0 16px",
          lineHeight: "normal",
          borderBottom: `1px solid ${token.colorBorderSecondary}`,
        }}
      >
        <TopBar onOpenSettings={() => setSettingsOpen(true)} />
      </Layout.Header>
      {/* 有侧栏时antd 会给 Layout 挂 `has-sider`，默认 row 布局。
          手柄必须排在 Sider **左边**（贴住分隔线），而它在 JSX 里写在 Sider 之后，
          所以这里用 `order` 明确前后，别依赖书写顺序。 */}
      <Layout style={{ minHeight: 0 }}>
        <Layout.Content style={{ position: "relative", minWidth: 0, order: 1 }}>
          <MapView selected={selected} onPickRider={pickRider} />
        </Layout.Content>
        {/* 手柄紧贴分隔线左侧，视觉上就是那条边框的「加粗版」。 */}
        <div
          className="sider-grip"
          role="separator"
          aria-orientation="vertical"
          aria-label="拖动调整侧栏宽度"
          aria-valuenow={sider}
          aria-valuemin={SIDER_MIN}
          aria-valuemax={SIDER_MAX}
          tabIndex={0}
          onPointerDown={startResize}
          onKeyDown={(e) => {
            if (e.key === "ArrowLeft") {
              setSider((w) => Math.min(SIDER_MAX, w + 24));
            } else if (e.key === "ArrowRight") {
              setSider((w) => Math.max(SIDER_MIN, w - 24));
            }
          }}
          onDoubleClick={() => setSider(SIDER_DEFAULT)}
          title={`侧栏 ${sider}px（双击复位，← → 键微调）`}
        />
        <Layout.Sider
          width={sider}
          theme="light"
          className="console-sider"
          style={{ order: 3 }}
        >
          <Tabs activeKey={tab} onChange={setTab} items={panels} />
        </Layout.Sider>
      </Layout>
      <SettingsModal open={settingsOpen} onClose={() => setSettingsOpen(false)} />
      <LoginGate open={role === null} />
    </Layout>
  );
}

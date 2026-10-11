import { useEffect, useState } from "react";
import { App as AntApp, Layout, Tabs, theme } from "antd";
import { useStore } from "./hooks";
import { AccessPanel } from "./components/AccessPanel";
import { AiPanel } from "./components/AiPanel";
import { LoginGate } from "./components/LoginGate";
import { LogPanel } from "./components/LogPanel";
import { MapView } from "./components/MapView";
import { OrdersPanel } from "./components/OrdersPanel";
import { RidersPanel } from "./components/RidersPanel";
import { SettingsPanel } from "./components/SettingsPanel";
import { TopBar } from "./components/TopBar";
import { store } from "./store";

export function App() {
  const { message } = AntApp.useApp();
  const { token } = theme.useToken();
  const role = useStore((s) => s.role);
  const [tab, setTab] = useState("riders");
  const [selected, setSelected] = useState<number | null>(null);

  // store 不直接依赖 antd，提示能力在挂载时注入。
  useEffect(() => {
    store.setNotifier((kind, text) => message[kind](text));
  }, [message]);

  const pickRider = (id: number) => {
    setSelected((cur) => (cur === id ? null : id));
    setTab("riders");
  };

  const panels = [
    { key: "riders", label: "骑手", children: <RidersPanel selected={selected} onSelect={pickRider} /> },
    { key: "orders", label: "订单", children: <OrdersPanel /> },
    { key: "ai", label: "AI", children: <AiPanel /> },
    { key: "access", label: "骑手接入", children: <AccessPanel /> },
    { key: "settings", label: "设置", children: <SettingsPanel /> },
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
        <TopBar />
      </Layout.Header>
      <Layout style={{ minHeight: 0 }}>
        <Layout.Content style={{ position: "relative", minWidth: 0 }}>
          <MapView selected={selected} onPickRider={pickRider} />
        </Layout.Content>
        <Layout.Sider width={420} theme="light" className="console-sider">
          <Tabs activeKey={tab} onChange={setTab} items={panels} />
        </Layout.Sider>
      </Layout>
      <LoginGate open={role === null} />
    </Layout>
  );
}

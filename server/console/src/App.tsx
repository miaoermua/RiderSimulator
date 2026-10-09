import { useEffect, useState } from "react";
import { App as AntApp, Layout, Tabs } from "antd";
import { useStore } from "./hooks";
import { LoginGate } from "./components/LoginGate";
import { LogPanel } from "./components/LogPanel";
import { MapView } from "./components/MapView";
import { OrdersPanel } from "./components/OrdersPanel";
import { RidersPanel } from "./components/RidersPanel";
import { SettingsPanel } from "./components/SettingsPanel";
import { TopBar } from "./components/TopBar";
import { store } from "./store";

const HEADER_STYLE: React.CSSProperties = {
  height: 48,
  lineHeight: "48px",
  padding: "0 16px",
  display: "flex",
  alignItems: "center",
  gap: 16,
  borderBottom: "1px solid #262c38",
};

export function App() {
  const { message } = AntApp.useApp();
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
    { key: "settings", label: "设置", children: <SettingsPanel /> },
    { key: "log", label: "日志", children: <LogPanel /> },
  ];

  return (
    <Layout style={{ height: "100vh" }}>
      <Layout.Header style={HEADER_STYLE}>
        <TopBar />
      </Layout.Header>
      <Layout style={{ minHeight: 0 }}>
        <Layout.Content style={{ position: "relative", minWidth: 0 }}>
          <MapView selected={selected} onPickRider={pickRider} />
        </Layout.Content>
        <Layout.Sider width={420} theme="dark" className="console-sider" style={{ borderLeft: "1px solid #262c38" }}>
          <Tabs activeKey={tab} onChange={setTab} items={panels} />
        </Layout.Sider>
      </Layout>
      <LoginGate open={role === null} />
    </Layout>
  );
}

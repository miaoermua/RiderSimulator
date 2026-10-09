import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App as AntApp, ConfigProvider } from "antd";
import zhCN from "antd/locale/zh_CN";
import "antd/dist/reset.css";
import "leaflet/dist/leaflet.css";
import "./styles.css";
import { App } from "./App";
import { consoleTheme } from "./theme";

const root = document.getElementById("root");
if (!root) throw new Error("缺少 #root 挂载点");

createRoot(root).render(
  <StrictMode>
    <ConfigProvider theme={consoleTheme} locale={zhCN}>
      {/* antd 的 App 组件提供 message/modal 的上下文，避免静态方法的主题告警。 */}
      <AntApp>
        <App />
      </AntApp>
    </ConfigProvider>
  </StrictMode>,
);

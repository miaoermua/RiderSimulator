import { theme, type ThemeConfig } from "antd";
import { ACCENT } from "./const";

/**
 * 把原来那套手写 CSS 变量映射成 antd token。
 *
 * `darkAlgorithm` 负责整体暗色兜底，token 负责把配色拉回原来的橙黑主题——
 * 否则 antd 默认的蓝紫会在暗色下显得很突兀。
 */
export const consoleTheme: ThemeConfig = {
  algorithm: theme.darkAlgorithm,
  token: {
    colorPrimary: ACCENT,
    colorBgBase: "#0f1218",
    colorBgContainer: "#171b23",
    colorBgElevated: "#1b1f27",
    colorBgLayout: "#0f1218",
    colorBorder: "#262c38",
    colorBorderSecondary: "#262c38",
    colorText: "#e6e9ef",
    colorTextSecondary: "#8a93a6",
    colorSuccess: "#2ecc71",
    colorWarning: "#f5b041",
    colorError: "#e74c3c",
    colorInfo: "#3d8bfd",
    borderRadius: 6,
    fontSize: 13,
    fontFamily: '-apple-system, "PingFang SC", "Microsoft YaHei", sans-serif',
  },
  components: {
    Layout: {
      headerBg: "#171b23",
      bodyBg: "#0f1218",
      siderBg: "#171b23",
    },
    Card: {
      colorBgContainer: "#10141b",
    },
    Tabs: {
      itemColor: "#8a93a6",
    },
  },
};

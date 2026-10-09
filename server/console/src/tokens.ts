import { theme } from "antd";
import { consoleTheme } from "./theme";

/**
 * 把设计令牌导出成 CSS 变量，供 `styles.css` 消费。
 *
 * 规范里三令五申「不要硬编码 `#FFFFFF` / `#FAFAFA`，角色才是关键」——
 * 但 Leaflet 与 `divIcon` 拼接出来的 HTML 不归 React 管，antd 组件
 * 也覆盖不到它们。官方给出的出路是 `theme.getDesignToken()`：
 * 在 React 之外读取当前主题产出的 Alias Token，再写进 `:root`。
 *
 * 在 `createRoot().render()` **之前**调用，首帧就有变量，
 * 不会出现「先白后灰」的闪一下。
 *
 * 变量前缀用 `--rs-`（rider-sim）而不是 `--ant-`，
 * 免得和 antd 自己的内部变量撞名。
 */
const VARS: ReadonlyArray<readonly [string, string]> = [
  ["--rs-bg-layout", "colorBgLayout"],
  ["--rs-bg-container", "colorBgContainer"],
  ["--rs-bg-elevated", "colorBgElevated"],
  ["--rs-border-secondary", "colorBorderSecondary"],
  ["--rs-text", "colorText"],
  ["--rs-text-tertiary", "colorTextTertiary"],
  ["--rs-shadow", "boxShadowTertiary"],
  ["--rs-font", "fontFamily"],
  ["--rs-font-mono", "fontFamilyCode"],
  ["--rs-text-light-solid", "colorTextLightSolid"],
];

export function applyDesignTokenVars(): void {
  const token = theme.getDesignToken(consoleTheme) as unknown as Record<string, unknown>;
  const root = document.documentElement.style;

  for (const [name, key] of VARS) {
    root.setProperty(name, String(token[key]));
  }
  // 圆角与字号是数值，消费方要自己补单位。
  root.setProperty("--rs-radius", `${token.borderRadius}px`);
  root.setProperty("--rs-font-size-sm", `${token.fontSizeSM}px`);
}

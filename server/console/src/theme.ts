import { theme, type ThemeConfig } from "antd";
import {
  COLOR_ERROR,
  COLOR_PRIMARY,
  COLOR_SUCCESS,
  COLOR_WARNING,
  FONT_STACK,
} from "./const";

/**
 * Ant Design 视觉语言的主题配置。
 *
 * 只做两件事：
 *
 * 1. **换种子**——主色、四个语义色、字体栈、基准字号、圆角。
 *    贴片、hover、active、outline 等派生色由 `defaultAlgorithm` 展开，
 *    不手写第二套梯度。
 * 2. **把 Layout 拉回三层表面模型**——antd 的 `headerBg` / `siderBg`
 *    默认是深蓝 `#001529`，与亮色表面模型冲突，必须显式指回容器白。
 *
 * 不使用 `darkAlgorithm`：底图是高德 / OSM 的亮色瓦片，暗色外壳会与地图打架。
 * 需要暗色时整包换成 `darkAlgorithm` 即可，算法会自己翻转表面阶梯与阴影。
 */
export const consoleTheme: ThemeConfig = {
  algorithm: theme.defaultAlgorithm,
  token: {
    colorPrimary: COLOR_PRIMARY,
    colorInfo: COLOR_PRIMARY,
    colorSuccess: COLOR_SUCCESS,
    colorWarning: COLOR_WARNING,
    colorError: COLOR_ERROR,
    fontFamily: FONT_STACK,
    fontSize: 14,
    borderRadius: 6,
  },
  components: {
    Layout: {
      // bg-layout：页面底板，包围并承载其余一切。
      bodyBg: "#F5F5F5",
      // bg-container：页头与侧栏是内容面，不是抬升面。
      headerBg: "#FFFFFF",
      siderBg: "#FFFFFF",
      headerPadding: "0 16px",
    },
    // 抬升面靠阴影而非颜色区分，卡片阴影收紧一档。
    Card: { paddingLG: 24 },
  },
};

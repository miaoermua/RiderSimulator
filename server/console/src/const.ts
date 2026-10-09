import type { CoordSystem, OrderStatus, Phase, Weather } from "./protocol";

/**
 * 设计令牌基线。
 *
 * 这里只放**种子值**：品牌主色、语义色与字体栈。派生梯度（hover / active /
 * outline / 浅色底）全部交给 antd 的 `defaultAlgorithm` 展开，
 * 不在产品代码里手写第二套颜色。
 *
 * 取值口径与 Ant Design 规范一致：主色 `#1677FF`，语义色 success `#52C41A`、
 * warning `#FAAD14`、error `#FF4D4F`，正文基准 14px。
 */

/** 品牌主色。用于动作、链接、焦点环与选中态。 */
export const COLOR_PRIMARY = "#1677FF";

export const COLOR_SUCCESS = "#52C41A";
export const COLOR_WARNING = "#FAAD14";
export const COLOR_ERROR = "#FF4D4F";

/**
 * Ant Design 字体栈：按序回退到系统 UI 字体，Linux 由 Noto Sans 覆盖。
 * 界面只用 400 / 600 两个字重，强调靠颜色与描边而非字重。
 */
export const FONT_STACK =
  '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, "Noto Sans", sans-serif';

/** 等宽字体：时间戳与坐标这类需要列对齐的数字。 */
export const MONO_STACK =
  '"SFMono-Regular", Consolas, "Liberation Mono", Menlo, Courier, monospace';

export const SPEEDS = [1, 2, 4, 8, 10, 60] as const;

export const WEATHERS: ReadonlyArray<readonly [Weather, string]> = [
  ["clear", "晴"],
  ["cloudy", "多云"],
  ["overcast", "阴"],
  ["fog", "雾"],
  ["light_rain", "小雨"],
  ["moderate_rain", "中雨"],
  ["heavy_rain", "大雨"],
  ["windy", "大风"],
  ["snow", "雪"],
  ["rainstorm", "暴雨"],
];

export const PHASE_LABEL: Record<Phase, string> = {
  to_shop: "去商家",
  waiting_food: "等餐",
  to_customer: "送餐中",
  arrived_customer: "已到顾客",
};

/**
 * 配送阶段的分类色。
 *
 * 取 Ant Design 预置调色板——规范里预置色**只用于标签、图表与分类可视化**，
 * 地图图层正属于此列，不得拿去染交互元素。
 */
export const PHASE_COLOR: Record<Phase, string> = {
  to_shop: "#1677FF",
  waiting_food: "#FAAD14",
  to_customer: "#52C41A",
  arrived_customer: "#722ED1",
};

/** 订单线颜色，同样来自预置调色板。 */
export const ORDER_COLOR: Record<string, string> = {
  pending: "#FAAD14",
  assigned: "#1677FF",
  picked_up: "#52C41A",
  late: "#FF4D4F",
};

/** 真人骑手单独一色，与 AI 骑手的阶段色区分开。 */
export const HUMAN_RIDER_COLOR = "#722ED1";

export const STATUS_LABEL: Record<OrderStatus, string> = {
  pending: "待抢",
  assigned: "已接单",
  picked_up: "配送中",
  delivered: "已送达",
  late: "超时",
  expired: "流单",
  cancelled: "取消",
};

export const SPEED_MODE_LABEL: Record<string, string> = {
  compliant: "合规",
  tolerant: "超速",
  unrestricted: "解限速",
};

export const COORD_LABEL: Record<CoordSystem, string> = {
  gcj02: "GCJ-02（高德 / 腾讯）",
  wgs84: "WGS-84（GPS / OSM）",
};

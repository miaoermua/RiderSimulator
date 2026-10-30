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

/* ---------------------------------------------------------------------------
 * 控件尺寸基线
 *
 * 以前每个组件各写各的高度：`Title level=4` 是 32、`level=5` 是 28、
 * `Tag` 是 24、`Button` 是 32、带文字的 `Badge` 是 22，靠 `align="center"`
 * 视觉上凑齐——实际是五种高度排一行，上下沿都不对齐。
 *
 * 现在统一到这里：**一行的所有控件都占 CONTROL_H**，需要"看起来小一点"的
 * 控件（Tag）用 `CONTROL_H - TAG_INSET` 并让父容器负责居中，而不是各写各的。
 * 32 这个值不是随手取的：antd 的 `size="middle"`（Button / Input / Select
 * 的默认档）就是 32，跟它对齐后原生控件不用额外覆盖。
 * ------------------------------------------------------------------------- */

/** 单行控件的基准高度。与 antd `middle` 档一致。 */
export const CONTROL_H = 32;

/**
 * Tag 比基准高度矮的量。
 *
 * Tag 自带 1px 边框 + 上下内边距，撑到 32 时视觉上比同行的 Button 高一线；
 * 压掉 4px 再让父容器（`inline-flex` + `align-items:center`）居中，
 * 边框才和按钮的中线重合。
 */
export const TAG_INSET = 4;

/** 同一行内不同控件组之间的间距。 */
export const GAP_INLINE = 16;

/** 同一组内元素之间的紧凑间距。 */
export const GAP_TIGHT = 8;

/**
 * Card 之间的垂直间距，按**语义**分两档，不要随手写第三个值：
 *
 * - `GAP_INLINE`（16）：两个不同区块之间，让人一眼看出"这是两件事"；
 * - `GAP_TIGHT`（8）：同一区块内紧贴上一块的附属卡片。
 *
 * 组内卡片堆叠（AI 面板那五张）另有约定：12，比区块间距紧、比紧凑间距松。
 */
export const CARD_GAP_BLOCK = 16;
export const CARD_GAP_STACK = 12;

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

import type { CoordSystem, OrderStatus, Phase, Weather } from "./protocol";

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

export const PHASE_COLOR: Record<Phase, string> = {
  to_shop: "#3d8bfd",
  waiting_food: "#f5b041",
  to_customer: "#2ecc71",
  arrived_customer: "#9b59b6",
};

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

/** 主题色。和原手写控制台保持一致，改成 antd token 后仍是这一支橙。 */
export const ACCENT = "#ff7a1a";

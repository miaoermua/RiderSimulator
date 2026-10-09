import type { CoordSystem } from "./protocol";

/**
 * 底图瓦片源。
 *
 * 坐标系决定用哪家底图——这是「切换坐标系」必须**跟着换底图**的原因：
 * GCJ-02 的坐标叠在 WGS-84 的底图上，在中国境内会整体偏移几百米。
 *
 * | 坐标系 | 底图 | 授权 |
 * |---|---|---|
 * | GCJ-02 | 高德网页瓦片 | 未授权直连，仅本地演示用 |
 * | WGS-84 | OpenStreetMap | ODbL，需遵守使用条款（限速、规范 UA） |
 */
export interface Basemap {
  id: CoordSystem;
  label: string;
  url: string;
  subdomains?: string;
  attribution: string;
  maxZoom: number;
}

export const BASEMAPS: Record<CoordSystem, Basemap> = {
  gcj02: {
    id: "gcj02",
    label: "高德",
    url: "https://webrd0{s}.is.autonavi.com/appmaptile?lang=zh_cn&size=1&scale=1&style=8&x={x}&y={y}&z={z}",
    subdomains: "1234",
    attribution: "© 高德地图",
    maxZoom: 18,
  },
  wgs84: {
    id: "wgs84",
    label: "OpenStreetMap",
    url: "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
    attribution: "© OpenStreetMap contributors",
    maxZoom: 19,
  },
};

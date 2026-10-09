import { useEffect, useRef, useState } from "react";
import L from "leaflet";
import { BASEMAPS } from "../basemaps";
import { PHASE_COLOR, PHASE_LABEL, STATUS_LABEL } from "../const";
import type { Order, RiderBrief } from "../protocol";
import { store } from "../store";
import { useStore } from "../hooks";

interface Props {
  selected: number | null;
  onPickRider: (id: number) => void;
}

/** 成都市中心，和 `sim::world` 的默认商圈一致。 */
const DEFAULT_CENTER: [number, number] = [30.66, 104.06];

const ORDER_COLOR: Record<string, string> = {
  pending: "#f5b041",
  assigned: "#3d8bfd",
  picked_up: "#2ecc71",
  late: "#e74c3c",
};

/**
 * Leaflet 图层视图。
 *
 * Leaflet 是命令式 API，组件库托管不了，所以这里用 `useState` 持有 map 实例
 * 让 effect 之间有明确的先后顺序；重绘订阅的是**快照**而非 tick，
 * 因此 60x 档下地图不会每 16ms 被重建一次。
 */
export function MapView({ selected, onPickRider }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const tileRef = useRef<L.TileLayer | null>(null);
  const pickRef = useRef(onPickRider);
  pickRef.current = onPickRider;

  const [map, setMap] = useState<L.Map | null>(null);
  const snap = useStore((s) => s.snap);
  const coord = useStore((s) => s.snap?.config.coord_system ?? "gcj02");

  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;
    const m = L.map(el, { zoomControl: true, attributionControl: true }).setView(DEFAULT_CENTER, 13);
    setMap(m);
    return () => {
      m.remove();
      setMap(null);
    };
  }, []);

  // 底图跟随坐标系：GCJ-02 用高德，WGS-84 用 OSM。
  // 换系不换图会整体偏移几百米，这是切换必须成对做的原因。
  useEffect(() => {
    if (!map) return;
    if (tileRef.current) map.removeLayer(tileRef.current);
    const bm = BASEMAPS[coord] ?? BASEMAPS.gcj02;
    tileRef.current = L.tileLayer(bm.url, {
      subdomains: bm.subdomains ?? "abc",
      maxZoom: bm.maxZoom,
      attribution: bm.attribution,
    });
    tileRef.current.addTo(map);
    return () => {
      if (tileRef.current) map.removeLayer(tileRef.current);
      tileRef.current = null;
    };
  }, [map, coord]);

  // 首屏把镜头拉到商户范围，之后不再自动移动，免得跟用户抢视角。
  useEffect(() => {
    if (!map || !snap || snap.merchants.length === 0 || store.getState().mapFitted) return;
    const bounds = L.latLngBounds(snap.merchants.map((m) => [m.lat, m.lng] as [number, number]));
    map.fitBounds(bounds.pad(0.2));
    store.markMapFitted();
  }, [map, snap]);

  // 图层整体重建。骑手与订单都是个位数，重建比增量 diff 更简单也更不易错。
  useEffect(() => {
    if (!map || !snap) return;
    const group = L.layerGroup();

    for (const m of snap.merchants) {
      L.circleMarker([m.lat, m.lng], {
        radius: 4,
        color: "#8a93a6",
        weight: 1,
        fillColor: "#8a93a6",
        fillOpacity: 0.7,
      })
        .bindTooltip(`${m.name} · ${m.signature}`)
        .addTo(group);
    }

    const drawOrder = (o: Order) => {
      const color = ORDER_COLOR[o.status] ?? "#f5b041";
      L.polyline(
        [
          [o.merchant_lat, o.merchant_lng],
          [o.customer_lat, o.customer_lng],
        ],
        { color, weight: 2, dashArray: o.status === "pending" ? "4 4" : undefined, opacity: 0.85 },
      )
        .bindTooltip(`#${o.id} ${o.merchant_name} · ${STATUS_LABEL[o.status]} · ${(o.distance_m / 1000).toFixed(1)}km`)
        .addTo(group);
      L.circleMarker([o.customer_lat, o.customer_lng], {
        radius: 3,
        color,
        weight: 1,
        fillColor: color,
        fillOpacity: 0.9,
      }).addTo(group);
    };

    snap.hall.forEach(drawOrder);
    snap.active_orders.forEach(drawOrder);

    for (const r of snap.riders) {
      const color = r.kind === "human" ? "#9b59b6" : r.phase ? PHASE_COLOR[r.phase] : "#8a93a6";
      const isSelected = selected === r.id;
      const icon = L.divIcon({
        className: "",
        html:
          `<div class="rider-icon${r.online ? "" : " off"}"` +
          ` style="background:${color};${isSelected ? "border-color:#ff7a1a;box-shadow:0 0 0 3px rgba(255,122,26,.45)" : ""}">` +
          `${r.name.slice(0, 1)}</div>`,
        iconSize: [26, 26],
        iconAnchor: [13, 13],
      });
      L.marker([r.lat, r.lng], { icon, zIndexOffset: isSelected ? 1000 : 0 })
        .bindTooltip(riderTooltip(r), { direction: "top", offset: [0, -14] })
        .on("click", () => pickRef.current(r.id))
        .addTo(group);
    }

    group.addTo(map);
    return () => {
      map.removeLayer(group);
    };
  }, [map, snap, selected]);

  return <div className="console-map" ref={hostRef} />;
}

function riderTooltip(r: RiderBrief): string {
  const bits = [r.name];
  if (r.phase) bits.push(PHASE_LABEL[r.phase]);
  if (r.held.length > 0) bits.push(`持单 ${r.held.length}`);
  if (!r.online) bits.push("离线");
  if (r.frozen) bits.push("冻结");
  return bits.join(" · ");
}

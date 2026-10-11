import { useEffect, useRef, useState } from "react";
import L from "leaflet";
import {
  App as AntApp,
  Button,
  Input,
  InputNumber,
  Modal,
  Radio,
  Space,
  Typography,
} from "antd";
import { BASEMAPS } from "../basemaps";
import { COLOR_PRIMARY, COLOR_SUCCESS } from "../const";
import { useIsControl, useStore } from "../hooks";
import type { CoordSystem } from "../protocol";
import { store } from "../store";

/** 武汉光谷，与 `sim::world` 的商圈中心一致（MapView 的兜底视角同源）。 */
const FALLBACK_CENTER: [number, number] = [30.5077, 114.4154];

/** 金额（元）的可填区间，与服务端 `world::MIN/MAX_CUSTOM_FEE_CENTS` 一致。 */
const MIN_FEE_YUAN = 1;
const MAX_FEE_YUAN = 2000;

type End = "merchant" | "customer";

/**
 * 「临时创建订单」入口。
 *
 * 放进订单面板头部：造出来的单立刻就进大厅列表，两者挨着才看得见因果。
 * 表单状态只在打开时重置——所以这里不用 `useState(初始值)` 从快照推，
 * 而是每次 `openForm()` 现算一份默认值，免得快照刷进来时把用户的草稿冲掉。
 */
export function CreateOrderButton() {
  const isControl = useIsControl();
  const snap = useStore((s) => s.snap);
  const { message } = AntApp.useApp();

  const [open, setOpen] = useState(false);
  const [merchantName, setMerchantName] = useState("");
  const [merchant, setMerchant] = useState<[number, number]>(FALLBACK_CENTER);
  const [customerName, setCustomerName] = useState("");
  const [customerAddr, setCustomerAddr] = useState("");
  const [customer, setCustomer] = useState<[number, number]>(FALLBACK_CENTER);
  const [feeYuan, setFeeYuan] = useState<number | null>(null);
  const [active, setActive] = useState<End>("merchant");

  const coord: CoordSystem = snap?.config.coord_system ?? "gcj02";

  const openForm = () => {
    const m = snap?.merchants[0];
    const base: [number, number] = m ? [m.lat, m.lng] : FALLBACK_CENTER;
    setMerchantName(m?.name ?? "");
    setMerchant(base);
    // 终点默认拉开一点：两端重合算不出距离，承诺时限也没有意义。
    setCustomer([round6(base[0] + 0.012), round6(base[1] + 0.01)]);
    setCustomerName("");
    setCustomerAddr("");
    setFeeYuan(null);
    setActive("merchant");
    setOpen(true);
  };

  const submit = () => {
    const name = merchantName.trim();
    if (!name) {
      message.error("商家名字不能空着");
      return;
    }
    if (samePoint(merchant, customer)) {
      message.error("商家和顾客不能是同一个点");
      return;
    }
    if (feeYuan != null && (feeYuan < MIN_FEE_YUAN || feeYuan > MAX_FEE_YUAN)) {
      message.error(`金额要在 ${MIN_FEE_YUAN}~${MAX_FEE_YUAN} 元之间`);
      return;
    }
    store.createOrder({
      merchant_name: name,
      merchant_lat: merchant[0],
      merchant_lng: merchant[1],
      ...(customerName.trim() ? { customer_name: customerName.trim() } : {}),
      ...(customerAddr.trim() ? { customer_address: customerAddr.trim() } : {}),
      customer_lat: customer[0],
      customer_lng: customer[1],
      ...(feeYuan != null ? { fee_cents: Math.round(feeYuan * 100) } : {}),
    });
    setOpen(false);
  };

  return (
    <>
      <Button type="primary" disabled={!isControl} onClick={openForm}>
        临时创建订单
      </Button>
      <Modal
        open={open}
        title="临时创建订单"
        width={620}
        okText="创建"
        cancelText="取消"
        onOk={submit}
        onCancel={() => setOpen(false)}
        destroyOnClose
      >
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <Typography.Text type="secondary">
            在地图上点一下就能摆点；先选「点谁」，再点地图。金额留空 = 按计价规则算。
          </Typography.Text>

          <Radio.Group
            value={active}
            onChange={(e) => setActive(e.target.value as End)}
            optionType="button"
            buttonStyle="solid"
          >
            <Radio.Button value="merchant">摆商家点</Radio.Button>
            <Radio.Button value="customer">摆顾客点</Radio.Button>
          </Radio.Group>

          {open && (
            <PickMap
              coord={coord}
              active={active}
              merchant={merchant}
              customer={customer}
              onPick={(end, pos) => (end === "merchant" ? setMerchant(pos) : setCustomer(pos))}
            />
          )}

          <Field label="商家名字">
            <Input
              value={merchantName}
              placeholder="例如：老街烧烤"
              onChange={(e) => setMerchantName(e.target.value)}
            />
          </Field>
          <Field label="商家坐标">
            <CoordInputs value={merchant} onChange={setMerchant} />
          </Field>
          <Field label="顾客名字" hint="留空 = 随机生成">
            <Input value={customerName} onChange={(e) => setCustomerName(e.target.value)} />
          </Field>
          <Field label="顾客地址" hint="留空 = 「商家名附近」">
            <Input value={customerAddr} onChange={(e) => setCustomerAddr(e.target.value)} />
          </Field>
          <Field label="顾客坐标">
            <CoordInputs value={customer} onChange={setCustomer} />
          </Field>
          <Field label="金额（元）" hint={`${MIN_FEE_YUAN}~${MAX_FEE_YUAN}；留空 = 按计价规则`}>
            <InputNumber
              min={MIN_FEE_YUAN}
              max={MAX_FEE_YUAN}
              step={0.5}
              precision={2}
              placeholder="自动"
              value={feeYuan}
              onChange={(v) => setFeeYuan(v ?? null)}
            />
          </Field>
        </Space>
      </Modal>
    </>
  );
}

/** 选点地图：商家 / 顾客两个圆点，点空白处把「当前那端」挪过去。 */
function PickMap({
  coord,
  active,
  merchant,
  customer,
  onPick,
}: {
  coord: CoordSystem;
  active: End;
  merchant: [number, number];
  customer: [number, number];
  onPick: (end: End, pos: [number, number]) => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [map, setMap] = useState<L.Map | null>(null);
  const fitted = useRef(false);
  const pickRef = useRef(onPick);
  pickRef.current = onPick;
  const activeRef = useRef(active);
  activeRef.current = active;

  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;
    const m = L.map(el, { zoomControl: true }).setView(merchant, 14);
    setMap(m);
    // 弹窗有入场动画，创建时容器尺寸还是 0；动画结束后补一次尺寸计算，
    // 否则瓦片会按 0×0 去切，看起来就是一片空白。
    const t = window.setTimeout(() => m.invalidateSize(), 220);
    return () => {
      window.clearTimeout(t);
      m.remove();
      setMap(null);
    };
    // 只建一次：底图随坐标系变化由下面的 effect 负责。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!map) return;
    const bm = BASEMAPS[coord] ?? BASEMAPS.gcj02;
    const layer = L.tileLayer(bm.url, {
      subdomains: bm.subdomains ?? "abc",
      maxZoom: bm.maxZoom,
      attribution: bm.attribution,
    }).addTo(map);
    return () => {
      map.removeLayer(layer);
    };
  }, [map, coord]);

  useEffect(() => {
    if (!map) return;
    const onClick = (e: L.LeafletMouseEvent) =>
      pickRef.current(activeRef.current, [e.latlng.lat, e.latlng.lng]);
    map.on("click", onClick);
    return () => {
      map.off("click", onClick);
    };
  }, [map]);

  useEffect(() => {
    if (!map) return;
    const group = L.layerGroup().addTo(map);
    L.circleMarker(merchant, {
      radius: 8,
      color: COLOR_PRIMARY,
      weight: 2,
      fillColor: COLOR_PRIMARY,
      fillOpacity: 0.85,
    })
      .bindTooltip("商家（起点）")
      .addTo(group);
    L.circleMarker(customer, {
      radius: 8,
      color: COLOR_SUCCESS,
      weight: 2,
      fillColor: COLOR_SUCCESS,
      fillOpacity: 0.85,
    })
      .bindTooltip("顾客（终点）")
      .addTo(group);
    L.polyline([merchant, customer], { color: "#8C8C8C", weight: 2, dashArray: "4 4" }).addTo(group);
    // 只在首次把镜头框到两点；之后用户怎么拖都不再抢视角。
    if (!fitted.current) {
      map.fitBounds(L.latLngBounds([merchant, customer]).pad(0.5));
      fitted.current = true;
    }
    return () => {
      map.removeLayer(group);
    };
  }, [map, merchant, customer]);

  return <div className="pick-map" ref={hostRef} />;
}

/** 经纬度数字对，用于地图点不准时手改。 */
function CoordInputs({
  value,
  onChange,
}: {
  value: [number, number];
  onChange: (v: [number, number]) => void;
}) {
  return (
    <Space>
      <InputNumber
        style={{ width: 150 }}
        step={0.0001}
        precision={6}
        value={value[0]}
        addonBefore="纬"
        onChange={(v) => v != null && onChange([v, value[1]])}
      />
      <InputNumber
        style={{ width: 150 }}
        step={0.0001}
        precision={6}
        value={value[1]}
        addonBefore="经"
        onChange={(v) => v != null && onChange([value[0], v])}
      />
    </Space>
  );
}

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div style={{ marginBottom: 4 }}>
        <Typography.Text>{label}</Typography.Text>
        {hint && (
          <Typography.Text type="secondary" style={{ marginInlineStart: 8, fontSize: 12 }}>
            {hint}
          </Typography.Text>
        )}
      </div>
      {children}
    </div>
  );
}

function samePoint(a: [number, number], b: [number, number]): boolean {
  return Math.abs(a[0] - b[0]) < 1e-6 && Math.abs(a[1] - b[1]) < 1e-6;
}

function round6(v: number): number {
  return Math.round(v * 1e6) / 1e6;
}

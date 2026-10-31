import { useEffect, useRef, useState } from "react";
import {
  Alert,
  App as AntApp,
  AutoComplete,
  Button,
  Divider,
  Empty,
  Input,
  InputNumber,
  Segmented,
  Select,
  Slider,
  Space,
  Switch,
  Typography,
} from "antd";
import { COORD_LABEL, WEATHERS } from "../const";
import { useIsControl, useStore } from "../hooks";
import { PanelHeading } from "./PanelHeading";
import type { CoordSystem } from "../protocol";
import { store, yuan } from "../store";

/** 三层模型的可配字段：机器名 → 中文名。 */
const LAYER_MODEL_FIELDS: Array<[string, string]> = [
  ["rider", "骑手"],
  ["station", "站长"],
  ["director", "烽火台"],
];

/**
 * 模型选择框。
 *
 * 为什么不直接用 antd `AutoComplete`：它**没有下拉箭头**，候选要点进输入框才弹出来，
 * 于是「共 27 个可选模型」这句话对用户等于没说——看不出这里能选。
 * 为什么不直接用 `Select`：`Select` 一旦有 `options` 就**不能自由输入**，
 * 而上游可能不给 `/models` 接口（404、鉴权、企业网关屏蔽），那时用户连模型名都填不进去。
 *
 * 所以两者取长补短：**候选够用时给一个带箭头的 `Select`**，
 * **候选为空（或用户想填清单外的名字）时退回 `AutoComplete`**。
 * 两种形态都保留「能打字」，这是这个控件的底线。
 */
function ModelPicker({
  value,
  options,
  onChange,
  disabled,
  error,
  allowClear,
}: {
  value: string;
  options: Array<{ value: string; label: string }>;
  onChange: (v: string) => void;
  disabled?: boolean;
  /** 上游拉取失败的原因；`null` = 没失败。类型对齐 store 的 `ModelListState`。 */
  error?: string | null;
  allowClear?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const errText = error || null;

  if (options.length === 0) {
    // 没有候选：保留输入框，但**自动展开空面板**并说清原因，
    // 用户就不会在这里反复点、怀疑控件坏了。
    return (
      <AutoComplete
        style={{ width: "100%" }}
        disabled={disabled}
        allowClear={allowClear}
        value={value}
        options={[]}
        open={open}
        onOpenChange={setOpen}
        onChange={onChange}
        placeholder="模型名"
        notFoundContent={errText ? `上游拉取失败：${errText}` : "上游没给模型清单，可直接手填"}
      >
        <Input
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder="模型名，如 deepseek-chat"
          suffix={
          <span
            aria-hidden
            style={{ color: "rgba(0,0,0,0.25)", cursor: "pointer" }}
            onClick={() => setOpen((v) => !v)}
          >
            ▾
          </span>
        }
        />
      </AutoComplete>
    );
  }

  return (
    <Select
      showSearch
      allowClear={allowClear}
      disabled={disabled}
      open={open}
      onOpenChange={setOpen}
      placeholder="点箭头选，或直接输入"
      value={value || undefined}
      options={options}
      // showSearch 自己会过滤；这里再过一遍会把它筛空。
      filterOption={() => true}
      onChange={(v) => onChange(v ?? "")}
      // 允许填清单外的模型名（上游刚上新、或清单被截断）。
      onSearch={(s) => {
        if (s && !options.some((o) => o.value === s)) onChange(s);
      }}
      style={{ width: "100%" }}
    />
  );
}

/**
 * 设置面板。
 *
 * 两部分：控制项（每次快照都可重建，直接读 store）与接入配置
 * （骑手名单 / LLM，含输入框，必须自己持有草稿状态，
 * 否则每秒一次的 resync 会把用户打到一半的字冲掉）。
 */
export function SettingsPanel() {
  const snap = useStore((s) => s.snap);
  const info = useStore((s) => s.info);
  const isControl = useIsControl();
  const { modal, message } = AntApp.useApp();

  if (!snap) return <Empty style={{ marginTop: 48 }} description="等待快照…" />;
  const c = snap.config;

  const llmEnabled = info?.llm_enabled ?? c.llm_brain_enabled;

  return (
    <div className="console-scroll">
      {!isControl && (
        <Alert
          type="warning"
          showIcon
          message="当前是观战身份，设置只读。"
          style={{ marginBottom: 16 }}
        />
      )}

      <PanelHeading>显示</PanelHeading>
      <Row label="地图坐标系" hint="切换后底图跟着换：GCJ-02 用高德，WGS-84 用 OpenStreetMap">
        <Segmented
          disabled={!isControl}
          value={c.coord_system ?? "gcj02"}
          options={[
            { label: "GCJ-02", value: "gcj02" },
            { label: "WGS-84", value: "wgs84" },
          ]}
          onChange={(v) => store.send("set_config", { coord_system: v as CoordSystem })}
        />
      </Row>
      <Row label="坐标系说明">
        <Typography.Text type="secondary">{COORD_LABEL[c.coord_system ?? "gcj02"]}</Typography.Text>
      </Row>

      <PanelHeading>AI</PanelHeading>
      <Row label="AI 骑手数量" hint="0~8；减少时手上有单的先跑完">
        <Space>
          <InputNumber
            min={0}
            max={8}
            disabled={!isControl}
            value={c.sim_rider_count}
            onChange={(v) => v != null && store.send("set_config", { sim_rider_count: v })}
          />
          <Button
            disabled={!isControl || c.sim_rider_count >= 8}
            onClick={() =>
              store.send("set_config", { sim_rider_count: Math.min(8, c.sim_rider_count + 1) })
            }
          >
            ＋ 加一个
          </Button>
        </Space>
      </Row>
      <Row
        label="算法每骑手上限"
        hint="同一轮里一个 AI 骑手最多吃几单；1 = 一轮一单"
      >
        <InputNumber
          min={1}
          max={3}
          disabled={!isControl}
          value={c.merge_max_per_rider}
          onChange={(v) => v != null && store.send("set_config", { merge_max_per_rider: v })}
        />
      </Row>
      <Row
        label="单店未完成上限"
        hint="同一家店同时压着几单未完成；0 = 不限。压太多谁也合不成"
      >
        <InputNumber
          min={0}
          max={6}
          disabled={!isControl}
          value={c.merge_merchant_cap}
          onChange={(v) => v != null && store.send("set_config", { merge_merchant_cap: v })}
        />
      </Row>
      <Row
        label="凑单窗口"
        hint="在店等餐的骑手愿为同店新单多等几分钟；0 = 不等，直接按原计划走"
      >
        <InputNumber
          min={0}
          max={10}
          disabled={!isControl}
          value={c.merge_wait_min}
          onChange={(v) => v != null && store.send("set_config", { merge_wait_min: v })}
        />
      </Row>
      <Row
        label="LLM 战术大脑"
        hint={
          info
            ? `今日调用 ${info.brain_calls_today ?? 0}/${info.brain_max_calls_per_day ?? "-"}${
                info.brain_note ? ` · ${info.brain_note}` : ""
              }`
            : "无 Key 时自动退回算法"
        }
      >
        <Switch
          checked={llmEnabled}
          disabled={!isControl}
          onChange={(v) => store.send("set_llm_enabled", { enabled: v })}
        />
      </Row>
      <SwitchRow
        label="AI 批量调度"
        hint="算法统一给 AI 骑手派单；关掉后 AI 只抢单"
        value={c.ai_batch_enabled}
        disabled={!isControl}
        onChange={(v) => store.send("set_config", { ai_batch_enabled: v })}
      />

      <PanelHeading>世界</PanelHeading>
      <Row label="天气">
        <Select
          style={{ width: 120 }}
          disabled={!isControl}
          value={snap.weather}
          options={WEATHERS.map(([v, n]) => ({ value: v, label: n }))}
          onChange={(v) => store.send("set_weather", { weather: v })}
        />
      </Row>
      <Row label="每日单量上限" hint="0 = 不限">
        <InputNumber
          min={0}
          disabled={!isControl}
          value={c.max_orders_per_day}
          onChange={(v) => store.send("set_config", { max_orders_per_day: Math.max(0, v ?? 0) })}
        />
      </Row>
      <Row label="世界种子">
        <Typography.Text type="secondary">{c.seed}</Typography.Text>
      </Row>

      {/* 单价的账要能对得起来：给的（起步 + 里程 + 加成）、乘的（高峰）、
          抽的（平台），一样一行摊开。只留一个「抽水」滑块等于把另外三样藏了。 */}
      <PanelHeading>计价</PanelHeading>
      <Row label="起步价" hint="含第一档里程；单位：元">
        <InputNumber
          min={0}
          max={20}
          step={0.5}
          disabled={!isControl}
          value={money(c.fee.base_fee_cents)}
          onChange={(v) => store.send("set_config", { base_fee_cents: toCents(v) })}
        />
      </Row>
      {/* 这一行右边不是控件而是一整句话（阶梯全貌有好几档），
          所以走 `stack` 给整行宽度——挤在两栏的右半边里只会换行成一片。 */}
      <Row label="里程阶梯" hint="整段总价，不是每段累加；区间左闭右开" stack>
        <Typography.Text type="secondary">
          {tierRanges(c.fee.distance_tiers, c.fee.per_km_over_cents)}
        </Typography.Text>
      </Row>
      <Row label="超出后每公里" hint="超出最后一档的单价；单位：元/km">
        <InputNumber
          min={0}
          max={10}
          step={0.1}
          disabled={!isControl}
          value={money(c.fee.per_km_over_cents)}
          onChange={(v) => store.send("set_config", { per_km_over_cents: toCents(v) })}
        />
      </Row>
      <Row label="等级加成" hint="每升一级多给的配送费；单位：元/级">
        <InputNumber
          min={0}
          max={5}
          step={0.1}
          disabled={!isControl}
          value={money(c.fee.level_bonus_per_level_cents)}
          onChange={(v) => store.send("set_config", { level_bonus_per_level_cents: toCents(v) })}
        />
      </Row>
      <Row label="平台抽水" hint="0%~30%，只抽配送费，不抽雨补与重投补贴">
        <Slider
          style={{ width: 160 }}
          min={0}
          max={30}
          value={Math.round(c.commission_rate * 100)}
          disabled={!isControl}
          onChangeComplete={(v) => store.send("set_commission_rate", { rate: v / 100 })}
        />
      </Row>
      <Row label="当前高峰系数">
        <Typography.Text type="secondary">×{c.fee.surge_multiplier.toFixed(2)}</Typography.Text>
      </Row>
      <Row label="重投补贴" hint="没人接或骑手取消时加价重投，加满上限才流单">
        <Typography.Text type="secondary">
          每次 +¥{yuan(c.fee.redispatch_surcharge_step_cents)}，最多重投{" "}
          {c.fee.max_redispatch_attempts} 次
        </Typography.Text>
      </Row>

      <PanelHeading>规则</PanelHeading>
      {/* 派单范围：圆心是骑手**当前有效定位**，判的是「骑手 → 订单取货点」。
          等于半径算通过、超过才拒；骑手没有新鲜定位时大厅直接为空。
          上下限来自服务端 `sim::dispatch`（1~500），后台这里只是把它显出来——
          越界值服务端会拒（返回 400），不会静默改成边界值。 */}
      <Row label="派单范围半径（km）" hint="骑手只能看到取货点在此半径内的待抢单；1~500">
        <InputNumber
          min={1}
          max={500}
          step={10}
          addonAfter="km"
          disabled={!isControl}
          value={c.dispatch_radius_km}
          onChange={(v) => v != null && store.send("set_config", { dispatch_radius_km: v })}
        />
      </Row>
      {/* 推荐概率：只管"被标推荐"的概率，与订单是否被接单无关。
          0 = 关掉推荐标签；100 = 大厅里每张都标。服务端同样严格校验（>100 报错）。 */}
      <Row
        label="推荐单概率（%）"
        hint="订单被标上「推荐」的概率，不是被接单的概率；0 = 关闭"
      >
        <InputNumber
          min={0}
          max={100}
          step={10}
          addonAfter="%"
          disabled={!isControl}
          value={c.recommend_percent}
          onChange={(v) => v != null && store.send("set_config", { recommend_percent: v })}
        />
      </Row>
      <Row label="抢单窗口（秒）">
        <InputNumber
          min={1}
          max={10}
          disabled={!isControl}
          value={c.claim_window_secs}
          onChange={(v) => v != null && store.send("set_config", { claim_window_secs: v })}
        />
      </Row>
      <Row label="背单上限">
        <InputNumber
          min={1}
          max={5}
          disabled={!isControl}
          value={c.backorder_max}
          onChange={(v) => v != null && store.send("set_config", { backorder_max: v })}
        />
      </Row>
      <Row label="每日拒单上限" hint="0 = 不限；拒完额度后系统塞来的指派单也拒不掉">
        <InputNumber
          min={0}
          max={999}
          disabled={!isControl}
          value={c.max_rejects_per_day}
          onChange={(v) => store.send("set_config", { max_rejects_per_day: Math.max(0, v ?? 0) })}
        />
      </Row>
      <SwitchRow
        label="允许超速"
        hint="骑手可切到 45km/h 档"
        value={c.allow_speeding}
        disabled={!isControl}
        onChange={(v) => store.send("set_config", { allow_speeding: v })}
      />
      <SwitchRow
        label="允许解除限速"
        hint="关闭时已在该档的骑手会被降回合规"
        value={c.allow_unrestricted}
        disabled={!isControl}
        onChange={(v) => store.send("set_config", { allow_unrestricted: v })}
      />
      <SwitchRow
        label="允许真人违规操作"
        hint="打开后真人骑手可以人没到就点「到店 / 已送达」；关掉则必须骑到位置"
        value={c.allow_rider_violation}
        disabled={!isControl}
        onChange={(v) => store.send("set_config", { allow_rider_violation: v })}
      />
      <Row
        label="取送半径（米）"
        hint="真人坐标进圈就能点到店 / 送达；0 = 关闭。与上面的违规开关互不影响"
      >
        <InputNumber
          min={0}
          max={2000}
          step={10}
          disabled={!isControl}
          value={c.arrive_radius_m}
          onChange={(v) => v != null && store.send("set_config", { arrive_radius_m: v })}
        />
      </Row>

      {/* 事故率：档位基准率（0.5% / 4% / 22%）是文档定死的，倍率只管手感。
          默认 1.0 = 设计值，嫌吵就往左拉。 */}
      <PanelHeading>事故</PanelHeading>
      <SwitchRow
        label="骑手会出事故"
        hint="关掉就完全不掷事故（演示 / 排查用）"
        value={c.accident_enabled}
        disabled={!isControl}
        onChange={(v) => store.send("set_config", { accident_enabled: v })}
      />
      <Row
        label="事故率倍率"
        hint="1.0 = 设计值（合规 0.5% / 超速 4% / 解除限速 22%）；往左更安全"
      >
        <Slider
          style={{ width: 160 }}
          min={0}
          max={300}
          step={10}
          value={Math.round(c.accident_rate_multiplier * 100)}
          disabled={!isControl || !c.accident_enabled}
          onChangeComplete={(v) => store.send("set_config", { accident_rate_multiplier: v / 100 })}
        />
        <Typography.Text type="secondary">×{c.accident_rate_multiplier.toFixed(1)}</Typography.Text>
      </Row>

      {/* 情绪：低到 lazy_below 之下才开始懒——挑单（门槛上浮）+ 磨洋工（速度打折）。 */}
      <PanelHeading>情绪</PanelHeading>
      <SwitchRow
        label="骑手有情绪"
        hint="罚款、超时、在线久了都掉；收工和日结回血。情绪低就变懒"
        value={c.mood_enabled}
        disabled={!isControl}
        onChange={(v) => store.send("set_config", { mood_enabled: v })}
      />
      <Row label="懒惰强度" hint="情绪见底时，接单时薪门槛最多上浮几倍（1.0 = 不影响接单）">
        <Slider
          style={{ width: 160 }}
          min={100}
          max={200}
          step={5}
          value={Math.round(c.mood_lazy_threshold_max * 100)}
          disabled={!isControl || !c.mood_enabled}
          onChangeComplete={(v) => store.send("set_config", { mood_lazy_threshold_max: v / 100 })}
        />
        <Typography.Text type="secondary">×{c.mood_lazy_threshold_max.toFixed(2)}</Typography.Text>
      </Row>

      <SwitchRow
        label="允许背单"
        hint="顺路时可以同时揣多单"
        value={c.backorder_enabled}
        disabled={!isControl}
        onChange={(v) => store.send("set_config", { backorder_enabled: v })}
      />
      <SwitchRow
        label="歧视性抽水"
        hint="按骑手画像差别抽成"
        value={c.discriminative_commission}
        disabled={!isControl}
        onChange={(v) => store.send("set_config", { discriminative_commission: v })}
      />
      <SwitchRow
        label="给骑手显示期望值"
        value={c.show_expected_value}
        disabled={!isControl}
        onChange={(v) => store.send("set_config", { show_expected_value: v })}
      />

      <AdvancedSection disabled={!isControl} onToast={message.info} onConfirm={modal.confirm} />

      {info?.hub && (
        <>
          <PanelHeading>服务器</PanelHeading>
          <Typography.Text type="secondary" style={{ wordBreak: "break-all" }}>
            {JSON.stringify(info.hub)}
          </Typography.Text>
        </>
      )}
    </div>
  );
}

/**
 * 接入配置：骑手名单 + LLM。
 *
 * 输入框持有草稿，快照刷新时只在「未被编辑过」的情况下回填，
 * 保证每秒一次的快照不会打断输入。
 */
function AdvancedSection({
  disabled,
  onToast,
  onConfirm,
}: {
  disabled: boolean;
  onToast: (text: string) => void;
  onConfirm: (cfg: { title: string; content: string; onOk: () => void }) => void;
}) {
  const snap = useStore((s) => s.snap);
  const info = useStore((s) => s.info);
  const models = useStore((s) => s.models);
  const [names, setNames] = useState("");
  const namesDirty = useRef(false);
  const [key, setKey] = useState("");
  const [base, setBase] = useState("");
  const [model, setModel] = useState("");
  const [layerModels, setLayerModels] = useState<Record<string, string>>({});
  const baseTouched = useRef(false);
  const modelTouched = useRef(false);
  const layerTouched = useRef(false);

  // 名单：从快照同步，用户改过就不再覆盖。
  useEffect(() => {
    if (!snap || namesDirty.current) return;
    setNames(
      snap.riders
        .filter((r) => r.kind !== "human")
        .sort((a, b) => a.id - b.id)
        .map((r) => r.name)
        .join("\n"),
    );
  }, [snap]);

  useEffect(() => {
    if (baseTouched.current) return;
    setBase(info?.llm_base_url ?? "");
  }, [info]);

  useEffect(() => {
    if (modelTouched.current) return;
    setModel(info?.llm_model ?? "");
  }, [info]);

  // 进面板就拉一次上游模型清单——下拉框有值才不至于手打模型名打错。
  // 观战身份调不了（服务端只认控制端），所以只在有权限时问。
  useEffect(() => {
    if (!disabled) store.listModels();
  }, [disabled]);

  // 分层模型：从最近一次模型清单回填，用户改过就不再覆盖。
  useEffect(() => {
    if (layerTouched.current || !models) return;
    setLayerModels(models.current ?? {});
  }, [models]);

  /** 上游给的模型名做成下拉候选；输入框仍允许自由填（上游可能不给 models 接口）。 */
  const modelOptions: Array<{ value: string; label: string }> = (models?.models ?? [])
    // 上游偶尔给出空 id 或非字符串（见过 `{id: null}` 的脏数据）。
    // 过滤掉而不是让它们进 options，否则 Select 会拿 undefined 当 value。
    .filter((m): m is typeof m & { id: string } => typeof m?.id === "string" && m.id !== "")
    .map((m) => ({ value: m.id, label: m.id }));

  const saveNames = () => {
    const list = names
      .split("\n")
      .map((s) => s.trim())
      .filter(Boolean);
    if (list.length === 0) {
      onToast("名单是空的：至少写一个名字");
      return;
    }
    store.send("set_rider_names", { names: list });
    namesDirty.current = false;
  };

  const saveLlm = () => {
    const body: Record<string, unknown> = { base_url: base.trim(), model: model.trim() };
    if (key.trim()) body.api_key = key.trim(); // 留空 = 不碰已存的 Key
    // 分层模型：只发这三个已知层；空串 = 该层回退到全局模型。
    body.layer_models = Object.fromEntries(
      LAYER_MODEL_FIELDS.map(([k]) => [k, (layerModels[k] ?? "").trim()]),
    );
    store.send("set_llm_config", body);
    setKey("");
  };

  return (
    <>
      <PanelHeading>骑手名单</PanelHeading>
      <Row label="AI 骑手名字" hint="一行一个，按骑手 id 顺序覆盖；空行跳过，重启后仍在" stack>
        <Input.TextArea
          rows={4}
          disabled={disabled}
          placeholder={"张三\n李四\n王五"}
          value={names}
          onChange={(e) => {
            namesDirty.current = true;
            setNames(e.target.value);
          }}
        />
        <Button disabled={disabled} onClick={saveNames} style={{ marginTop: 8 }}>
          应用名单
        </Button>
      </Row>

      <PanelHeading>LLM 接入</PanelHeading>
      <Row
        label="API Key"
        hint={info?.llm_configured ? `已配置（${info.llm_model || "?"}）` : "未配置：填一个 Key 才能用 LLM 大脑"}
        stack
      >
        <Input.Password
          disabled={disabled}
          autoComplete="new-password"
          placeholder="sk-…（留空 = 不改）"
          value={key}
          onChange={(e) => setKey(e.target.value)}
        />
      </Row>
      <Row label="Base URL" hint="OpenAI 兼容端点的根，一般到 /v1 为止；留空 = 用默认" stack>
        <Input
          disabled={disabled}
          placeholder="https://api.openai.com/v1"
          value={base}
          onChange={(e) => {
            baseTouched.current = true;
            setBase(e.target.value);
          }}
        />
      </Row>
      <Row label="模型" hint="三层没有单独指定时都用它；点右侧箭头看候选" stack>
        <ModelPicker
          disabled={disabled}
          value={model}
          options={modelOptions}
          error={models?.error}
          onChange={(v) => {
            modelTouched.current = true;
            setModel(v);
          }}
        />
      </Row>

      <Row
        label="上游模型列表"
        hint={
          models?.loading
            ? "拉取中…"
            : models?.error
              ? `拉取失败：${models.error}`
              : models
                ? `共 ${models.models.length} 个模型，可在上方「模型」与各层模型里点箭头选择`
                : "进面板时自动拉取"
        }
      >
        <Button disabled={disabled} loading={models?.loading} onClick={() => store.listModels()}>
          刷新
        </Button>
      </Row>

      {LAYER_MODEL_FIELDS.map(([field, label]) => (
        <Row
          key={field}
          label={`${label}层模型`}
          hint={`留空 = 用全局模型${modelOptions.length ? "；点箭头看候选" : ""}`}
          stack
        >
          <ModelPicker
            disabled={disabled}
            allowClear
            value={layerModels[field] ?? ""}
            options={modelOptions}
            error={models?.error}
            onChange={(v) => {
              layerTouched.current = true;
              setLayerModels((d) => ({ ...d, [field]: v }));
            }}
          />
        </Row>
      ))}

      <Space style={{ marginTop: 8 }}>
        <Button disabled={disabled} onClick={saveLlm}>
          保存
        </Button>
        <Button
          danger
          disabled={disabled}
          onClick={() =>
            onConfirm({
              title: "清空 API Key？",
              content: "清空后 LLM 战术大脑会自动关闭。",
              onOk: () => store.send("set_llm_config", { api_key: "" }),
            })
          }
        >
          清除 Key
        </Button>
      </Space>
      <Divider style={{ margin: "16px 0 8px" }} />{/* 接入配置与运行参数分段 */}
    </>
  );
}


function Row({
  label,
  hint,
  stack,
  children,
}: {
  label: string;
  hint?: string;
  stack?: boolean;
  children: React.ReactNode;
}) {
  return (
    <div className={`set-row${stack ? " stack" : ""}`}>
      <div>
        <div>{label}</div>
        {hint && <Typography.Text type="secondary">{hint}</Typography.Text>}
      </div>
      <div className="set-ctl">{children}</div>
    </div>
  );
}

function SwitchRow({
  label,
  hint,
  value,
  disabled,
  onChange,
}: {
  label: string;
  hint?: string;
  value: boolean;
  disabled: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <Row label={label} hint={hint}>
      <Switch checked={value} disabled={disabled} onChange={onChange} />
    </Row>
  );
}

/** 计费项协议用「分」，界面用「元」，来回都得过一手。 */
function money(cents: number): number {
  return cents / 100;
}

function toCents(yuanValue: number | null): number {
  return Math.max(0, Math.round((yuanValue ?? 0) * 100));
}

/**
 * 把里程阶梯念成「几公里到几公里加多少钱」。
 *
 * 三处必须和 `economy::distance_extra_cents` 对齐，否则界面会说谎：
 *
 * 1. **区间是左闭右开**。门槛值算进高档，所以 1.5km 整就该是 +¥1.00，
 *    念成「<1.5km 不加价 · 1.5~3.0km」正好对；用 `<1.5km` 而非 `≤1.5km`
 *    就会让人以为 1.5km 不加价。
 *
 * 2. **加价未变的相邻档可以合并**，但**末档不能合并掉上界**。
 *    默认配置 `(5000,350) (8000,350)`：8km 是「超出后按每公里叠加」的分界，
 *    合并成一句「5.0km 起 +¥3.50」会让人以为 8km 之后也是 3.50 平的——
 *    实际 8.5km 是 3.80，10km 是 4.70。所以末档一律带上界，
 *    把「超出后每公里」交给下一行那个输入框说。
 *
 * 3. **首档之前不是 0**，除非首档门槛就是 0。默认首档 1500m 表示
 *    「1.5km 以内不加里程费」，但起步价本身另算，不能混为一谈。
 */
function tierRanges(tiers: [number, number][], perKmCents: number): string {
  if (tiers.length === 0) return "—";
  const km = (m: number) => (m / 1000).toFixed(1);
  const yuanOf = (c: number) => (c / 100).toFixed(2);

  // 合并加价相同的相邻档，但**永远保留最后一档**——它带着「超出后叠加」的分界。
  const steps: Array<[number, number]> = [];
  for (const [m, cents] of tiers) {
    const prev = steps[steps.length - 1];
    const isLast = m === tiers[tiers.length - 1][0];
    if (prev && prev[1] === cents && !isLast) continue;
    steps.push([m, cents]);
  }

  const parts: string[] = [`≤${km(steps[0][0])}km 不加`];
  steps.forEach(([m, cents], i) => {
    const next = steps[i + 1];
    parts.push(
      next
        ? `${km(m)}~${km(next[0])}km +¥${yuanOf(cents)}`
        : `>${km(m)}km +¥${yuanOf(cents)}`,
    );
  });
  // 末档之上还有每公里叠加，说清楚它从哪儿开始，别让人按平价理解。
  if (perKmCents > 0) {
    parts.push(`超出部分 ¥${(perKmCents / 100).toFixed(2)}/km`);
  }
  return parts.join(" · ");
}

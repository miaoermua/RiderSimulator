import { useEffect, useRef, useState } from "react";
import {
  Alert,
  App as AntApp,
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
import type { CoordSystem } from "../protocol";
import { store } from "../store";

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

      <Heading>显示</Heading>
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

      <Heading>AI</Heading>
      <Row label="AI 骑手数量" hint="0~8；减少时手上有单的先跑完">
        <InputNumber
          min={0}
          max={8}
          disabled={!isControl}
          value={c.sim_rider_count}
          onChange={(v) => v != null && store.send("set_config", { sim_rider_count: v })}
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

      <Heading>世界</Heading>
      <Row label="天气">
        <Select
          style={{ width: 120 }}
          disabled={!isControl}
          value={snap.weather}
          options={WEATHERS.map(([v, n]) => ({ value: v, label: n }))}
          onChange={(v) => store.send("set_weather", { weather: v })}
        />
      </Row>
      <Row label="平台抽水" hint="0%~30%，只抽配送费">
        <Slider
          style={{ width: 160 }}
          min={0}
          max={30}
          value={Math.round(c.commission_rate * 100)}
          disabled={!isControl}
          onChangeComplete={(v) => store.send("set_commission_rate", { rate: v / 100 })}
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

      <Heading>规则</Heading>
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
          <Heading>服务器</Heading>
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
  const [names, setNames] = useState("");
  const namesDirty = useRef(false);
  const [key, setKey] = useState("");
  const [base, setBase] = useState("");
  const [model, setModel] = useState("");
  const baseTouched = useRef(false);
  const modelTouched = useRef(false);

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
    store.send("set_llm_config", body);
    setKey("");
  };

  return (
    <>
      <Heading>骑手名单</Heading>
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

      <Heading>LLM 接入</Heading>
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
      <Row label="Base URL" hint="OpenAI 兼容端点，留空 = 用默认" stack>
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
      <Row label="模型" stack>
        <Input
          disabled={disabled}
          placeholder="gpt-4o-mini"
          value={model}
          onChange={(e) => {
            modelTouched.current = true;
            setModel(e.target.value);
          }}
        />
      </Row>
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

/** 分组标题：`title-lg`（16px / 600）。标题就该是主文本色，次级色留给描述。 */
function Heading({ children }: { children: React.ReactNode }) {
  return (
    <Typography.Title level={5} style={{ marginTop: 24, marginBottom: 8 }}>
      {children}
    </Typography.Title>
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

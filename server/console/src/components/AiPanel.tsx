import { useEffect } from "react";
import {
  Alert,
  App as AntApp,
  Button,
  Card,
  Descriptions,
  Empty,
  Progress,
  Segmented,
  Space,
  Statistic,
  Switch,
  Tag,
  Timeline,
  Tooltip,
  Typography,
} from "antd";
import { useIsControl, useStore } from "../hooks";
import type { AiLayerState, HostMode } from "../protocol";
import { store, yuan, type AiDecisionEntry } from "../store";

/** 三层的中文名。`layer` 是协议里的机器名，界面上不该出现。 */
const LAYER_LABEL: Record<string, string> = {
  rider: "骑手 AI",
  station: "站长 AI",
  director: "烽火台 AI",
  algorithm: "算法",
  manual: "人工",
};

/** 各层在界面上用的标签色，方便一眼扫出「这波是谁在动」。 */
const LAYER_COLOR: Record<string, string> = {
  rider: "blue",
  station: "gold",
  director: "purple",
  algorithm: "default",
  manual: "green",
};

const HOST_MODE_HINT: Record<HostMode, string> = {
  off: "AI 全关：三层都不再调用，订单全靠人工接",
  simulated_only: "只服务模拟骑手；真人掉线后保持离线（默认）",
  auto_pilot: "真人掉线后由 AI 接管：替他取餐、送达，回来一连接立刻交还",
};

/**
 * 三层 AI 控制台。
 *
 * 三块：**开关与预算**（现在调不调、还能调几次）、**站点 KPI**（站长在优化什么）、
 * **决策流**（AI 刚才干了什么、为什么，以及能不能撤）。
 * 顺序刻意如此：先看「值不值得调」，再看「目标是什么」，最后看「具体动了什么」。
 */
export function AiPanel() {
  const ai = useStore((s) => s.ai);
  const decisions = useStore((s) => s.decisions);
  const isControl = useIsControl();

  // 打开面板主动拉一份最新状态；随后 server_info / ai_state 会自动续上。
  useEffect(() => {
    store.queryAi();
  }, []);

  if (!ai) {
    return <Empty style={{ marginTop: 48 }} description="等待 AI 状态…" />;
  }

  const disabled = !isControl || !ai.llm_configured;
  const used = ai.budget.calls_today;
  const max = Math.max(1, ai.budget.max_calls_per_day);
  const pct = Math.min(100, Math.round((used / max) * 100));

  return (
    <div className="console-scroll">
      {!isControl && (
        <Alert
          type="warning"
          showIcon
          message="当前是观战身份，AI 开关只读。"
          style={{ marginBottom: 16 }}
        />
      )}

      <Card size="small" title="总开关与预算" style={{ marginBottom: 12 }}>
        <Space direction="vertical" style={{ width: "100%" }} size={12}>
          <Space style={{ width: "100%", justifyContent: "space-between" }}>
            <Space direction="vertical" size={0}>
              <Typography.Text>允许调用 LLM</Typography.Text>
              <Typography.Text type="secondary">
                {ai.llm_configured
                  ? `已配置：${ai.llm_model || "默认模型"}`
                  : "未配置 Key，所有层都会回退到算法"}
              </Typography.Text>
            </Space>
            <Switch
              checked={ai.brain_enabled}
              disabled={!isControl}
              onChange={(v) => store.setAi({ enabled: v })}
            />
          </Space>

          <div>
            <Progress
              percent={pct}
              size="small"
              status={pct >= 100 ? "exception" : "normal"}
              format={() => `${used}/${max}`}
            />
            <Typography.Text type="secondary">
              今日跨层总调用。在飞：{ai.budget.inflight ? LAYER_LABEL[ai.budget.inflight] : "无"}
            </Typography.Text>
          </div>

          <Descriptions size="small" column={2}>
            <Descriptions.Item label="指纹缓存">
              {(ai.cache.hit_rate * 100).toFixed(0)}%（省下 {ai.cache.hits} 次）
            </Descriptions.Item>
            {(["rider", "station", "director"] as const).map((k) => (
              <Descriptions.Item key={k} label={LAYER_LABEL[k]}>
                {ai.models[k] || ai.llm_model || "默认"}
              </Descriptions.Item>
            ))}
          </Descriptions>
        </Space>
      </Card>

      <Card size="small" title="三层开关" style={{ marginBottom: 12 }}>
        {ai.layers.map((l) => (
          <LayerRow key={l.layer} info={l} disabled={disabled} />
        ))}
        <Typography.Text type="secondary" style={{ display: "block", marginTop: 8 }}>
          站长 AI 的限单 / 强派搭骑手层那一轮一起下发，暂不单独占坑。
        </Typography.Text>
      </Card>

      <Card size="small" title="真人骑手接管" style={{ marginBottom: 12 }}>
        <Segmented
          block
          disabled={!isControl}
          value={ai.host_mode}
          options={[
            { label: "不代管", value: "off" },
            { label: "只托管 AI 骑手", value: "simulated_only" },
            { label: "掉线即接管", value: "auto_pilot" },
          ]}
          onChange={(v) => store.setAi({ host_mode: v as HostMode })}
        />
        <Typography.Text type="secondary" style={{ display: "block", marginTop: 8 }}>
          {HOST_MODE_HINT[ai.host_mode]}
        </Typography.Text>
      </Card>

      <Card size="small" title={`站点 KPI · ${ai.station.name}`} style={{ marginBottom: 12 }}>
        <Space size={24} wrap>
          <Statistic
            title="准时率"
            value={(ai.station.kpi.on_time_rate * 100).toFixed(1)}
            suffix="%"
            valueStyle={ai.station.kpi.on_time_rate < 0.9 ? { color: "#CF1322" } : undefined}
          />
          <Statistic title="今日完成" value={ai.station.kpi.completed} suffix="单" />
          <Statistic title="超时" value={ai.station.kpi.late_count} suffix="单" />
          <Statistic
            title="平均时薪"
            value={yuan(ai.station.kpi.avg_hourly_cents)}
            prefix="¥"
          />
        </Space>
        <Descriptions size="small" column={2} style={{ marginTop: 12 }}>
          <Descriptions.Item label="活跃骑手">
            {ai.station.kpi.active_riders}/{ai.station.kpi.total_riders}
          </Descriptions.Item>
          <Descriptions.Item label="大厅积压">{ai.station.kpi.backlog} 单</Descriptions.Item>
          <Descriptions.Item label="平均等待">
            {ai.station.kpi.avg_wait_min.toFixed(1)} 分
          </Descriptions.Item>
        </Descriptions>
      </Card>

      <Card size="small" title="决策流" style={{ marginBottom: 12 }}>
        {decisions.length === 0 ? (
          <Typography.Text type="secondary">还没有 AI 决策（没配 Key 或还没到间隔）。</Typography.Text>
        ) : (
          <Timeline
            items={decisions.map((d) => ({
              color: LAYER_COLOR[d.layer] ?? "gray",
              children: <DecisionItem entry={d} />,
            }))}
          />
        )}
      </Card>

      {ai.reject_log.length > 0 && (
        <Card size="small" title="人工否决台账">
          <Space direction="vertical" size={4}>
            {ai.reject_log.slice(0, 10).map((r, i) => (
              <Typography.Text key={i} type="secondary">
                #{r.decision_id} {LAYER_LABEL[r.layer] ?? r.layer} ·{" "}
                {r.result === "reverted" ? "已撤销" : "已生效，仅记录"}
              </Typography.Text>
            ))}
          </Space>
        </Card>
      )}
    </div>
  );
}

function LayerRow({ info, disabled }: { info: AiLayerState; disabled: boolean }) {
  const label = LAYER_LABEL[info.layer] ?? info.layer;
  return (
    <div className="set-row">
      <div>
        <div>{label}</div>
        <Typography.Text type="secondary">
          每 {info.interval_min} 分最多一次 · 采纳 {info.applied} / 丢弃 {info.dropped} · 回退{" "}
          {info.fallbacks}
        </Typography.Text>
      </div>
      <div className="set-ctl">
        <Switch
          checked={info.enabled}
          disabled={disabled}
          onChange={(v) => store.setAi({ [info.layer]: v } as Parameters<typeof store.setAi>[0])}
        />
      </div>
    </div>
  );
}

function DecisionItem({ entry }: { entry: AiDecisionEntry }) {
  const { modal } = AntApp.useApp();
  const rejectable = useStore((s) => s.ai?.rejectable);
  const canReject =
    entry.decisionId > 0 && !entry.rejected && rejectable?.decision_id === entry.decisionId;

  return (
    <Space direction="vertical" size={2} style={{ width: "100%" }}>
      <Space size={6} wrap>
        <Tag color={LAYER_COLOR[entry.layer] ?? "default"}>
          {LAYER_LABEL[entry.layer] ?? entry.layer}
        </Tag>
        <Typography.Text type="secondary">{entry.at}</Typography.Text>
        {entry.cached && <Tag>缓存命中</Tag>}
        {entry.model && <Typography.Text type="secondary">{entry.model}</Typography.Text>}
        {entry.latencyMs != null && (
          <Typography.Text type="secondary">{entry.latencyMs}ms</Typography.Text>
        )}
        {entry.rejected && <Tag color="red">已否决</Tag>}
      </Space>

      {entry.note && <Typography.Text>{entry.note}</Typography.Text>}

      {entry.ops.map((o, i) => (
        <Typography.Text key={i} type="secondary">
          · {o.op} {o.target} {o.detail}
          {o.why ? ` — ${o.why}` : ""}
        </Typography.Text>
      ))}

      {entry.narrate && <Typography.Text italic>“{entry.narrate}”</Typography.Text>}

      {canReject && (
        <Tooltip title="撤销这条还没落地的决策">
          <Button
            size="small"
            danger
            onClick={() =>
              modal.confirm({
                title: "否决这条 AI 决策？",
                content: entry.note || "该决策还没落地，否决后不会执行。",
                onOk: () => store.rejectAi(entry.decisionId),
              })
            }
          >
            否决
          </Button>
        </Tooltip>
      )}
    </Space>
  );
}

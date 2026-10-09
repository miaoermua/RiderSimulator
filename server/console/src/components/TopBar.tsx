import { Badge, Button, Segmented, Space, Tag, Typography } from "antd";
import { SPEEDS } from "../const";
import { useStore } from "../hooks";
import { hhmm, nowMinuteOfDay, store, yuan } from "../store";

/**
 * 顶栏是唯一订阅 tick 的组件——60x 档下它每 16ms 重渲染一次，
 * 所以里面只能放便宜的东西。地图和列表读的是快照，不受影响。
 *
 * 视觉上遵循两条规范：倍率是**互斥选择**，交给 `Segmented`（选中项即
 * 全屏唯一的主色动作）；暂停与连接状态是**状态**，交给 `Badge` 传达，
 * 不用彩色正文也不占用主色。
 */
export function TopBar() {
  const role = useStore((s) => s.role);
  const conn = useStore((s) => s.conn);
  const snap = useStore((s) => s.snap);
  const tick = useStore((s) => s.tick);

  const isControl = role === "control";
  const c = snap?.config;
  const day = tick?.day ?? snap?.day ?? 0;
  const minute = tick?.sim_minute ?? snap?.sim_minute ?? 0;
  const paused = c?.paused ?? false;
  const realtime = c?.realtime ?? false;
  const speed = tick?.speed_multiplier ?? c?.speed_multiplier ?? 1;

  const resumeIfPaused = () => {
    if (paused) store.send("set_paused", { paused: false });
  };
  const pickSpeed = (v: number) => {
    resumeIfPaused();
    store.send("set_speed_multiplier", { multiplier: v });
  };
  const pickRealtime = () => {
    resumeIfPaused();
    // 真实钟表的分钟数由浏览器给出，服务端不引时区库。
    store.send("set_realtime", { enabled: true, wall_minute: nowMinuteOfDay() });
  };

  const runState = paused ? "paused" : realtime ? "realtime" : "running";

  return (
    <Space size={16} align="center" style={{ width: "100%" }}>
      <Typography.Title level={5} style={{ margin: 0, whiteSpace: "nowrap" }}>
        烽火台
      </Typography.Title>

      <Typography.Title
        level={4}
        style={{ margin: 0, whiteSpace: "nowrap", fontVariantNumeric: "tabular-nums" }}
      >
        第 {day} 天 {hhmm(minute)}
      </Typography.Title>

      <Badge
        status={runState === "paused" ? "warning" : "processing"}
        text={runState === "paused" ? "已暂停" : runState === "realtime" ? "实时" : "运行中"}
      />

      {snap && (
        <Space size={4}>
          <Tag style={{ marginInlineEnd: 0 }}>
            {snap.weather_label} ×{snap.weather_speed_factor.toFixed(2)}
          </Tag>
          {snap.rain_bonus_cents > 0 && (
            <Tag color="blue" style={{ marginInlineEnd: 0 }}>
              雨补 {yuan(snap.rain_bonus_cents)}
            </Tag>
          )}
          <Tag color={c?.is_peak ? "gold" : "default"} style={{ marginInlineEnd: 0 }}>
            {c?.is_peak ? `高峰 ×${c.surge_multiplier.toFixed(1)}` : "平峰"}
          </Tag>
        </Space>
      )}

      <Segmented
        value={realtime ? "realtime" : String(speed)}
        disabled={!isControl}
        onChange={(v) => (v === "realtime" ? pickRealtime() : pickSpeed(Number(v)))}
        options={[
          ...SPEEDS.map((v) => ({ label: `${v}x`, value: String(v) })),
          { label: "实时", value: "realtime" },
        ]}
      />

      <Button disabled={!isControl} onClick={() => store.send("set_paused", { paused: !paused })}>
        {paused ? "继续" : "暂停"}
      </Button>

      <Space size={16} style={{ marginInlineStart: "auto" }}>
        <Tag color={role === "control" ? "green" : "default"} style={{ marginInlineEnd: 0 }}>
          {role === "control" ? "控制端" : role === "spectator" ? "观战" : role === "rider" ? "骑手端" : "未连接"}
        </Tag>
        <Badge
          status={conn === "online" ? "success" : conn === "connecting" ? "processing" : "error"}
          text={conn === "online" ? "已连接" : conn === "connecting" ? "连接中" : "已断开"}
        />
      </Space>
    </Space>
  );
}

import { Badge, Button, Dropdown, Space, Tag, Tooltip, Typography, theme } from "antd";
import {
  PauseCircleOutlined,
  PlayCircleOutlined,
  SettingOutlined,
} from "@ant-design/icons";
import { CONTROL_H, GAP_INLINE, GAP_TIGHT, SPEEDS, TAG_INSET } from "../const";
import { useStore } from "../hooks";
import { hhmm, nowMinuteOfDay, store, yuan } from "../store";

/**
 * 顶栏是唯一订阅 tick 的组件——60x档下它每 16ms 重渲染一次，
 * 所以里面只能放便宜的东西。地图和列表读的是快照，不受影响。
 *
 * 变速做成**折叠**的下拉：档位有七个，摊在页头会把时间和状态挤到边上，
 * 而真正需要一眼看见的只有「当前跑多快」。运行状态交给 `Badge`，
 * 倍率交给下拉按钮，两者不重复表达同一件事。
 */

/**
 * 行高统一取自 `const.ts` 的 `CONTROL_H`。
 *
 * 这里曾经有一个本地的 `BAR_H = 32`，与弹窗、面板里的同类常量各写各的；
 * 改一个高度要翻三处。现在只留一个来源——顶栏和设置弹窗的量尺是同一把。
 */
const BAR_H = CONTROL_H;

/** 顶栏里反复出现的控件样式。抽出来是为了让高度只在一个地方定义。 */
const boxStyle = { height: BAR_H, display: "inline-flex", alignItems: "center" } as const;

/** Tag 比行高矮一档，靠父容器的居中对齐到同一中线。 */
const tagStyle = {
  ...boxStyle,
  height: BAR_H - TAG_INSET,
  lineHeight: `${BAR_H - TAG_INSET - 2}px`,
  marginInlineEnd: 0,
  fontVariantNumeric: "tabular-nums" as const,
} as const;

/**
 * 标识文字（「烽火台」）的样式。
 *
 * 时间已经从同一个 Title 里拆出去了（见下面 JSX 的说明），
 * 所以这里只剩标识本身——不再需要给两种字号找共同基线。
 */
const titleStyle = {
  margin: 0,
  lineHeight: `${BAR_H}px`,
  whiteSpace: "nowrap" as const,
};

export function TopBar({ onOpenSettings }: { onOpenSettings: () => void }) {
  const { token } = theme.useToken();
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

  // 下拉按钮上只写倍率本身；暂停与否由状态点说，避免同一件事说两遍。
  const speedLabel = realtime ? "实时" : `${speed}x`;

  /**
   * 一个状态点说完整件事，不再分「运行状态」和「连接状态」两颗。
   *
   * 之前顶栏有两个 Badge：左边「运行中 / 已暂停」，右边「已连接 / 已断开」。
   * 它们表达的是同一件事的三个层次——**连接是运行的前提**，断了就无所谓跑没跑，
   * 分成两颗反而要多看一处才知道"为什么时间不动了"。
   *
   * 优先级：连不上 > 暂停 > 跑着。断开时不再提"运行中"，
   * 因为那时的"运行中"没有意义。
   */
  const state: { status: "success" | "processing" | "warning" | "error"; text: string } =
    conn !== "online"
      ? {
          status: conn === "connecting" ? "processing" : "error",
          text: conn === "connecting" ? "连接中" : "未连接",
        }
      : paused
        ? { status: "warning", text: "已暂停" }
        : { status: "success", text: "运行中" };

  /**
   * 天气读数。
   *
   * 系数贴 1 时不写尾巴——「晴 ×1.00」里的乘数没有任何信息量，
   * 常态天气占着一块位置只会挤掉时间。真影响速度了（雨雪雾）才亮出来。
   */
  const wxFactor = snap?.weather_speed_factor ?? 1;
  const wxText = `${snap?.weather_label ?? ""}${
    Math.abs(wxFactor - 1) >= 0.005 ? ` ${wxFactor.toFixed(2)}×` : ""
  }`;

  const speedMenu = {
    selectable: true,
    selectedKeys: [realtime ? "realtime" : String(speed)],
    items: [
      { key: "realtime", label: "实时（1 模拟分钟 = 1 真实分钟）" },
      { type: "divider" as const },
      ...SPEEDS.map((v) => ({ key: String(v), label: `${v}x` })),
    ],
    onClick: ({ key }: { key: string }) =>
      key === "realtime" ? pickRealtime() : pickSpeed(Number(key)),
  };

  return (
    <Space size={GAP_INLINE} align="center" style={{ width: "100%", height: BAR_H, lineHeight: `${BAR_H}px` }}>
      <Typography.Title level={5} style={{ ...titleStyle, fontSize: 18, fontWeight: 600 }}>
        烽火台
      </Typography.Title>

      {/* 时间分两档：`第 N 天` 是背景信息，`HH:MM` 才是每次都要读的数。
          以前两者同字号挤在一个 Title 里，视线没有落点；拆开后
          小字天数退到次级色，时间保持 22px 等宽数字，读起来是一眼的事。 */}
      <span
        style={{
          display: "inline-flex",
          alignItems: "baseline",
          gap: 6,
          height: BAR_H,
          whiteSpace: "nowrap",
        }}
      >
        <span style={{ fontSize: 13, color: token.colorTextSecondary }}>第 {day} 天</span>
        <span
          style={{
            fontSize: 22,
            fontWeight: 600,
            lineHeight: `${BAR_H}px`,
            fontVariantNumeric: "tabular-nums",
          }}
        >
          {hhmm(minute)}
        </span>
      </span>

      <span style={boxStyle}>
        <Badge status={state.status} text={state.text} />
      </span>

      {snap && (
        <Space size={GAP_TIGHT} align="center" style={{ height: BAR_H }}>
          <Tooltip title="天气对骑行速度的影响。接近 1 表示没影响，不显示系数。">
            <Tag style={tagStyle}>{wxText}</Tag>
          </Tooltip>
          {snap.rain_bonus_cents > 0 && (
            <Tag color="blue" style={tagStyle}>
              雨补 {yuan(snap.rain_bonus_cents)}
            </Tag>
          )}
          {/* 平峰是常态，不占位。只有高峰才值得提示——那才是该多跑单的时候。 */}
          {c?.is_peak && (
            <Tag color="gold" style={tagStyle}>
              高峰 {c.surge_multiplier.toFixed(1)}×
            </Tag>
          )}
        </Space>
      )}

      <Dropdown menu={speedMenu} trigger={["click"]} disabled={!isControl}>
        <Button style={{ height: BAR_H }} disabled={!isControl}>
          <Space size={6}>
            变速
            <span style={{ fontVariantNumeric: "tabular-nums" }}>{speedLabel}</span>
            <span aria-hidden>▾</span>
          </Space>
        </Button>
      </Dropdown>

      {/* 暂停/继续做成图标：它是个开关，不是"执行某件事"，
          一个图标加 tooltip 比两个字的按钮更省地方，也不会和「变速」抢宽度。
          方框钉成 BAR_H，与左侧按钮同高。 */}
      <Tooltip title={paused ? "继续" : "暂停"}>
        <Button
          icon={paused ? <PlayCircleOutlined /> : <PauseCircleOutlined />}
          disabled={!isControl}
          onClick={() => store.send("set_paused", { paused: !paused })}
          aria-label={paused ? "继续" : "暂停"}
          style={{ width: BAR_H, height: BAR_H, minWidth: BAR_H }}
        />
      </Tooltip>

      <Space size={GAP_INLINE} align="center" style={{ marginInlineStart: "auto", height: BAR_H }}>
        <span style={boxStyle}>
          <Tag color={role === "control" ? "green" : "default"} style={tagStyle}>
            {role === "control" ? "控制端" : role === "spectator" ? "观战" : role === "rider" ? "骑手端" : "未连接"}
          </Tag>
        </span>
        {/* 连接状态不再单独一颗 Badge：已并入左侧的统一状态点
            （连不上 > 暂停 > 跑着）。同一件事说两遍只会让顶栏更挤。 */}
        {/* 设置入口。用 text 按钮而不是带文字的主按钮：
            它是低频动作，不该和「变速 / 暂停」抢注意力。
            方框尺寸钉成 BAR_H，边框对齐同一中线。 */}
        <Tooltip title="控制台设置">
          <Button
            type="text"
            icon={<SettingOutlined />}
            onClick={onOpenSettings}
            aria-label="控制台设置"
            style={{ width: BAR_H, height: BAR_H, minWidth: BAR_H }}
          />
        </Tooltip>
      </Space>
    </Space>
  );
}

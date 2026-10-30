import { useEffect, useState } from "react";
import {
  App as AntApp,
  Button,
  Card,
  Divider,
  Empty,
  Form,
  Input,
  InputNumber,
  Popconfirm,
  Progress,
  Select,
  Space,
  Switch,
  Tag,
  Typography,
} from "antd";
import { useIsControl, useStore } from "../hooks";
import type { ActivityView } from "../protocol";
import { hhmm, store, yuan } from "../store";

/**
 * 首页通知：烽火台写给骑手的一句话。
 *
 * 手机首页原来那条「在线 / 今日收入」的绿色条子已经换成它——不写就不显示。
 * **只有一条**：改了就覆盖，没有历史、没有列表，所以这里没有「新建」，
 * 只有「保存」和「清空」。可以挂一个活动，骑手点开通知时顺带看到自己的进度。
 */
function NoticeEditor({ activities }: { activities: ActivityView[] }) {
  const { message } = AntApp.useApp();
  const snap = useStore((s) => s.snap);
  const [form] = Form.useForm<{
    title: string;
    body: string;
    activity_id?: number;
  }>();
  const [busy, setBusy] = useState(false);
  const notice = snap?.notice ?? null;

  // 别人改了通知（或多个控制端同时开着）时跟着刷新；但**别覆盖正在打的字**。
  useEffect(() => {
    if (form.isFieldsTouched()) return;
    form.setFieldsValue({
      title: notice?.title ?? "",
      body: notice?.body ?? "",
      activity_id: notice?.activity_id ?? undefined,
    });
  }, [notice, form]);

  const save = async () => {
    let values: { title: string; body: string; activity_id?: number };
    try {
      values = await form.validateFields();
    } catch {
      return; // 校验失败，antd 已在字段下面写原因。
    }
    setBusy(true);
    try {
      await store.setNotice({
        title: values.title.trim(),
        body: values.body.trim(),
        activity_id: values.activity_id ?? null,
      });
      message.success("首页通知已更新，在线骑手立刻可见");
    } catch (e) {
      message.error((e as Error).message || "保存失败");
    } finally {
      setBusy(false);
    }
  };

  const clear = async () => {
    setBusy(true);
    try {
      await store.clearNotice();
      form.setFieldsValue({ title: "", body: "", activity_id: undefined });
      message.success("已清空，手机首页不再显示通知");
    } catch (e) {
      message.error((e as Error).message || "清空失败");
    } finally {
      setBusy(false);
    }
  };

  // 「第 N 天 HH:MM 发布」：时间按模拟钟算，控制台自己的时区不参与。
  const postedAt = (() => {
    if (!notice || !snap) return null;
    const minutesAgo = Math.max(0, snap.elapsed_min - notice.updated_at_min);
    const at = Math.max(0, snap.sim_minute - minutesAgo);
    return `第 ${Math.floor(at / 1440) + 1} 天 ${hhmm(at)}`;
  })();

  return (
    <Card
      size="small"
      style={{ marginBottom: 16 }}
      title="首页通知"
      extra={
        notice ? (
          <Tag color="orange">正在显示{postedAt ? ` · ${postedAt}` : ""}</Tag>
        ) : (
          <Tag>未设置</Tag>
        )
      }
    >
      <Form form={form} layout="vertical" disabled={busy}>
        <Form.Item
          name="title"
          label="标题"
          rules={[
            { required: true, whitespace: true, message: "写一句抓眼球的话" },
            { max: 40, message: "最多 40 个字（手机上只显示一行）" },
          ]}
        >
          <Input placeholder="例如：暴雨天注意安全" maxLength={40} showCount />
        </Form.Item>
        <Form.Item
          name="body"
          label="正文"
          rules={[
            { required: true, whitespace: true, message: "写清楚要骑手做什么" },
            { max: 500, message: "最多 500 个字" },
          ]}
          extra="骑手点开首页那条通知看全文；不设置就不显示这一条。"
        >
          <Input.TextArea
            rows={3}
            maxLength={500}
            showCount
            placeholder="例如：今天下午有雷雨，接单前看一眼路况；超时罚款今天减半。"
          />
        </Form.Item>
        <Form.Item
          name="activity_id"
          label="关联活动（可选）"
          extra="选一个活动后，骑手点开通知会看到自己在该活动上的进度。"
        >
          <Select
            allowClear
            placeholder="不关联活动"
            options={activities.map((a) => ({
              value: a.id,
              label: a.active ? a.title : `${a.title}（已下架）`,
            }))}
          />
        </Form.Item>
        <Space>
          <Button type="primary" loading={busy} onClick={save}>
            保存
          </Button>
          <Popconfirm
            title="清空首页通知？"
            description="立即从所有骑手首页消失，内容不留档。"
            okText="清空"
            cancelText="取消"
            disabled={!notice}
            onConfirm={clear}
          >
            <Button danger disabled={!notice}>
              清空
            </Button>
          </Popconfirm>
          <Typography.Text type="secondary">
            只有一条：保存即覆盖，没有历史
          </Typography.Text>
        </Space>
      </Form>
    </Card>
  );
}

/**
 * 平台活动：跑够 N 单给一笔奖金。
 *
 * 两条规则写在界面上，免得运营反复问：
 *
 * 1. **达成自动入账**——不用骑手点领取，也不走人工发放。
 * 2. **从建活动那一刻起算**——开启前跑的单不追溯，所有人从 0 开始。
 *
 * 列表数据来自快照（`snap.activities`）：服务端每帧下发，手机端与这里看的是
 * 同一份，不存在「控制台说在做、骑手看不到」的中间态。
 */
export function ActivitiesPanel() {
  const { message } = AntApp.useApp();
  const canControl = useIsControl();
  const snap = useStore((s) => s.snap);
  const [form] = Form.useForm<{
    title: string;
    target_orders: number;
    reward_yuan: number;
  }>();
  const [busy, setBusy] = useState(false);

  if (!canControl) {
    return <Empty description="观战模式看不到活动配置。" />;
  }

  const activities = snap?.activities ?? [];

  const create = async () => {
    let values: { title: string; target_orders: number; reward_yuan: number };
    try {
      values = await form.validateFields();
    } catch {
      return; // 校验失败，antd 已在字段下面写原因。
    }
    setBusy(true);
    try {
      await store.createActivity({
        title: values.title.trim(),
        target_orders: values.target_orders,
        reward_cents: Math.round(values.reward_yuan * 100),
      });
      message.success("活动已创建，达成即自动入账");
      form.resetFields();
    } catch (e) {
      message.error((e as Error).message || "创建失败");
    } finally {
      setBusy(false);
    }
  };

  const toggle = async (a: ActivityView, active: boolean) => {
    try {
      await store.setActivityActive(a.id, active);
      message.success(active ? "已重新上架" : "已下架，不再累计也不再发奖");
    } catch (e) {
      message.error((e as Error).message || "操作失败");
    }
  };

  const remove = async (a: ActivityView) => {
    try {
      await store.deleteActivity(a.id);
      message.success("已删除（已发的奖金不收回）");
    } catch (e) {
      message.error((e as Error).message || "删除失败");
    }
  };

  return (
    <div className="console-scroll">
      <NoticeEditor activities={activities} />

      <Card size="small" title="新建活动">
        <Form
          form={form}
          layout="vertical"
          initialValues={{ target_orders: 10, reward_yuan: 10 }}
          disabled={busy}
        >
          <Form.Item
            name="title"
            label="活动标题"
            rules={[
              { required: true, whitespace: true, message: "写一句骑手看得懂的话" },
              { max: 40, message: "最多 40 个字" },
            ]}
          >
            <Input placeholder="例如：午高峰冲单奖" maxLength={40} showCount />
          </Form.Item>
          <Space size="middle" align="start" wrap>
            <Form.Item
              name="target_orders"
              label="完成单数"
              rules={[{ required: true, message: "填一个正整数" }]}
              extra="活动开启后累计完成多少单算达成"
            >
              <InputNumber min={1} max={9999} precision={0} style={{ width: 140 }} />
            </Form.Item>
            <Form.Item
              name="reward_yuan"
              label="奖金"
              rules={[{ required: true, message: "填金额" }]}
              extra="达成后一次性打入今日到手"
            >
              <InputNumber
                min={0}
                max={100000}
                step={1}
                precision={2}
                addonAfter="元"
                style={{ width: 160 }}
              />
            </Form.Item>
          </Space>
          <Divider style={{ margin: "4px 0 12px" }} />
          <Space>
            <Button type="primary" loading={busy} onClick={create}>
              创建活动
            </Button>
            <Typography.Text type="secondary">
              达成自动入账，日志里记一条「活动奖励」
            </Typography.Text>
          </Space>
        </Form>
      </Card>

      <Divider orientation="left" plain>
        活动列表（{activities.length}）
      </Divider>

      {activities.length === 0 ? (
        <Empty description="还没有活动。" />
      ) : (
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          {activities.map((a) => {
            const progress = (snap?.riders ?? [])
              .map((r) => r.activities?.find((p) => p.activity_id === a.id))
              .filter((p): p is NonNullable<typeof p> => Boolean(p));
            const reached = progress.filter((p) => p.rewarded).length;
            return (
              <Card
                key={a.id}
                size="small"
                title={
                  <Space>
                    <span>{a.title}</span>
                    {a.active ? (
                      <Tag color="green">进行中</Tag>
                    ) : (
                      <Tag>已下架</Tag>
                    )}
                  </Space>
                }
                extra={
                  <Space>
                    <Switch
                      checked={a.active}
                      checkedChildren="上架"
                      unCheckedChildren="下架"
                      onChange={(v) => toggle(a, v)}
                    />
                    <Popconfirm
                      title="删除这个活动？"
                      description="已发的奖金不收回，骑手进度一并作废。"
                      okText="删除"
                      cancelText="取消"
                      onConfirm={() => remove(a)}
                    >
                      <Button danger size="small">
                        删除
                      </Button>
                    </Popconfirm>
                  </Space>
                }
              >
                <Space split={<Divider type="vertical" />} wrap>
                  <span>
                    目标 <b>{a.target_orders}</b> 单
                  </span>
                  <span>
                    奖金 <b>{yuan(a.reward_cents)}</b> 元
                  </span>
                  <span>
                    已达成 <b>{reached}</b> / 参与的 {progress.length} 人
                  </span>
                </Space>
                <Progress
                  style={{ marginTop: 8 }}
                  percent={
                    progress.length === 0
                      ? 0
                      : Math.round((reached / progress.length) * 100)
                  }
                  size="small"
                  status={a.active ? "active" : "normal"}
                />
              </Card>
            );
          })}
        </Space>
      )}
    </div>
  );
}

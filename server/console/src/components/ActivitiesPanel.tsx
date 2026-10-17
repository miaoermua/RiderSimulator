import { useState } from "react";
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
  Space,
  Switch,
  Tag,
  Typography,
} from "antd";
import { useIsControl, useStore } from "../hooks";
import type { ActivityView } from "../protocol";
import { store, yuan } from "../store";

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
    <div className="panel-scroll">
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

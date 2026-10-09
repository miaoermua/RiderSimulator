import { useState } from "react";
import { Alert, Button, Input, Modal, Space, Typography } from "antd";
import { useStore } from "../hooks";
import { store } from "../store";

/**
 * 进入控制台前的令牌闸门。
 *
 * 服务端在 `RIDER_SIM_ALLOW_CONTROL` 打开且令牌匹配时才给 `control` 角色，
 * 否则只授予观战。这里只负责把令牌递过去，鉴权结果由 welcome 事件回报。
 */
export function LoginGate({ open }: { open: boolean }) {
  const conn = useStore((s) => s.conn);
  const err = useStore((s) => s.lastError);
  const [token, setToken] = useState(() => sessionStorage.getItem("fh_token") ?? "");

  const submit = () => {
    const t = token.trim();
    if (t) sessionStorage.setItem("fh_token", t);
    store.clearError();
    store.connect(t);
  };

  return (
    <Modal open={open} footer={null} closable={false} maskClosable={false} width={380} title="烽火台 · 控制台">
      <Space direction="vertical" style={{ width: "100%" }} size="middle">
        <Typography.Paragraph type="secondary" style={{ marginBottom: 0 }}>
          服务端开启 `RIDER_SIM_ALLOW_CONTROL` 时，填入与 `RIDER_SIM_CONTROL_TOKEN` 一致的令牌即可获得控制权；留空则只能观战。
        </Typography.Paragraph>
        <Input.Password
          placeholder="控制令牌（可留空观战）"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          onPressEnter={submit}
          autoFocus
        />
        {err && <Alert type="error" showIcon message={err} />}
        <Button type="primary" block loading={conn === "connecting"} onClick={submit}>
          进入
        </Button>
      </Space>
    </Modal>
  );
}

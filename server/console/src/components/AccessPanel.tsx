import { useCallback, useEffect, useState } from "react";
import {
  Alert,
  App as AntApp,
  Button,
  Card,
  Divider,
  Empty,
  Input,
  List,
  Popconfirm,
  Space,
  Tag,
  Typography,
} from "antd";
import { useIsControl, useStore } from "../hooks";
import type { Account, AccountJoin, AccountList, RiderBrief } from "../protocol";
import { store } from "../store";

/**
 * 骑手接入面板（烽火台）。
 *
 * 三件事：告诉管理员**手机该连哪个地址**、**给骑手建号**（建完当场出二维码）、
 * **管已有账号**（重置密码 / 删号）。
 *
 * ## 为什么走 HTTP 而不是 WS
 *
 * 账号是管理动作，不在 16ms 的帧流里。服务端把**明文密码只放在 HTTP 响应**里
 * （库里只有 Argon2 哈希），WS 那条路上根本没有它——这既是取舍也是保险。
 *
 * ## 二维码里是明文密码
 *
 * 扫一下即登录，代价是这张码能截图、能打印、**发出去收不回来**。想作废只能
 * 重置该骑手的密码（密码一变，旧码当场失效）。界面上把这句话写清楚。
 */
export function AccessPanel() {
  const isControl = useIsControl();
  const riders = useStore((s) => s.snap?.riders);

  const [list, setList] = useState<AccountList | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 刚建号 / 刚重置出来的那一份「唯一一次」材料。 */
  const [minted, setMinted] = useState<AccountJoin | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      setList(await store.listAccounts());
      setError(null);
    } catch (e) {
      setError(errText(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (isControl) void refresh();
  }, [isControl, refresh]);

  if (!isControl) {
    return (
      <div className="console-scroll">
        <Alert type="warning" showIcon message="当前是观战身份，骑手接入只读。" />
      </div>
    );
  }

  const primary = list?.primary_host ?? null;
  const hosts = list?.hosts ?? [];

  return (
    <div className="console-scroll">
      <Heading>手机可达地址</Heading>
      {primary ? (
        <AddressCard primary={primary} hosts={hosts} />
      ) : (
        <Typography.Text type="secondary">
          {loading ? "读取中…" : "服务端还没给出可达地址。"}
        </Typography.Text>
      )}

      <Heading>建号</Heading>
      <CreateForm
        onMinted={(a) => {
          setMinted(a);
          void refresh();
        }}
      />

      {minted && <MintedCard join={minted} onClose={() => setMinted(null)} />}

      <Divider style={{ margin: "16px 0 8px" }} />

      <Space style={{ marginBottom: 8 }} align="center">
        <Heading inline>账号名单</Heading>
        <Button size="small" loading={loading} onClick={() => void refresh()}>
          刷新
        </Button>
      </Space>
      {error && <Alert type="error" showIcon message={error} style={{ marginBottom: 8 }} />}

      {list && list.accounts.length === 0 ? (
        <Empty description="还没有账号：建一个，把二维码给骑手" />
      ) : (
        <List
          size="small"
          dataSource={list?.accounts ?? []}
          renderItem={(a) => (
            <AccountRow
              account={a}
              online={liveOnline(a, riders)}
              onReset={(join) => {
                setMinted(join);
                void refresh();
              }}
              onDeleted={() => void refresh()}
            />
          )}
        />
      )}

      <Typography.Paragraph type="secondary" style={{ marginTop: 16 }}>
        二维码里带着明文密码：扫一下即登录，但这张码能截图、能打印，发出去就收不回来。
        想作废只能重置该骑手的密码——旧码会当场失效。
      </Typography.Paragraph>
    </div>
  );
}

/** 手机该连的地址。主推一个，其余作为备选列出来。 */
function AddressCard({ primary, hosts }: { primary: string; hosts: string[] }) {
  const unreachable = isLoopback(primary);
  return (
    <Card size="small">
      <Space direction="vertical" size={4} style={{ width: "100%" }}>
        <Space align="center" wrap>
          <Typography.Text strong style={{ fontSize: 16 }}>
            {primary}
          </Typography.Text>
          <CopyButton text={primary} label="复制地址" />
        </Space>
        {unreachable && (
          <Alert
            type="warning"
            showIcon
            message="这是回环地址，手机连不上；请用局域网 IP 或域名（可设 RIDER_SIM_PUBLIC_ADDR）。"
          />
        )}
        {hosts.length > 1 && (
          <Typography.Text type="secondary">备选：{hosts.slice(1).join("、")}</Typography.Text>
        )}
      </Space>
    </Card>
  );
}

/** 建号表单：名字必填，密码留空则由服务端生成一个。 */
function CreateForm({ onMinted }: { onMinted: (a: AccountJoin) => void }) {
  const { message } = AntApp.useApp();
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    if (!name.trim()) return;
    setBusy(true);
    try {
      const a = await store.createAccount(name.trim(), password.trim() || undefined);
      setName("");
      setPassword("");
      message.success(`建了号「${a.name}」，把二维码给骑手`);
      onMinted(a);
    } catch (e) {
      // 重名冲突 / 密码太短都在这里。服务端文案已经写好了，直接透传。
      message.error(errText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Space direction="vertical" style={{ width: "100%" }} size={8}>
      <Input
        placeholder="骑手名字（= 账号名）"
        value={name}
        maxLength={12}
        onChange={(e) => setName(e.target.value)}
        onPressEnter={() => void submit()}
      />
      <Input.Password
        placeholder="初始密码（留空 = 服务端生成）"
        value={password}
        onChange={(e) => setPassword(e.target.value)}
        onPressEnter={() => void submit()}
      />
      <Button type="primary" loading={busy} disabled={!name.trim()} onClick={() => void submit()}>
        建号并出二维码
      </Button>
    </Space>
  );
}

/** 刚建号 / 刚重置出来的材料：二维码 + 明文密码。只此一次。 */
function MintedCard({ join, onClose }: { join: AccountJoin; onClose: () => void }) {
  const svg = join.join?.qr_svg ?? null;
  return (
    <Card
      size="small"
      style={{ marginTop: 12 }}
      title={`「${join.name}」的接入码`}
      extra={
        <Button type="link" size="small" onClick={onClose}>
          收起
        </Button>
      }
    >
      <Space direction="vertical" size={8} style={{ width: "100%" }}>
        <Alert
          type="warning"
          showIcon
          message="密码只显示这一次（库里只有哈希）。收起前请把二维码给骑手。"
        />
        {svg ? (
          <img
            alt={`登录二维码：${join.name}`}
            src={qrDataUrl(svg)}
            width={200}
            height={200}
            style={{ background: "#fff", padding: 8, borderRadius: 8 }}
          />
        ) : (
          <Typography.Text type="secondary">
            二维码画不出来，请把下面的连接串给骑手。
          </Typography.Text>
        )}
        <Field label="名字" value={join.name} />
        <Field label="密码" value={join.password} mono />
        {join.join && <Field label="服务器" value={join.join.ws_url} mono />}
        {join.join && <Field label="连接串" value={join.join.uri} mono small />}
      </Space>
    </Card>
  );
}

function Field({
  label,
  value,
  mono,
  small,
}: {
  label: string;
  value: string;
  mono?: boolean;
  small?: boolean;
}) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
      <Typography.Text type="secondary" style={{ flex: "0 0 56px" }}>
        {label}
      </Typography.Text>
      <Typography.Text
        code={mono}
        style={{ flex: 1, wordBreak: "break-all", fontSize: small ? 12 : undefined }}
      >
        {value}
      </Typography.Text>
      <CopyButton text={value} />
    </div>
  );
}

/** 名单里的一行。 */
function AccountRow({
  account,
  online,
  onReset,
  onDeleted,
}: {
  account: Account;
  online: boolean | null;
  onReset: (a: AccountJoin) => void;
  onDeleted: () => void;
}) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);

  const reset = async () => {
    setBusy(true);
    try {
      const a = await store.setAccountPassword(account.name);
      message.success(`重置了「${account.name}」的密码`);
      onReset(a);
    } catch (e) {
      message.error(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    try {
      const r = await store.deleteAccount(account.name);
      message.success(r.deleted ? `已删「${account.name}」` : `「${account.name}」本来就不在`);
      onDeleted();
    } catch (e) {
      message.error(errText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <List.Item
      actions={[
        <Button key="reset" type="link" size="small" loading={busy} onClick={() => void reset()}>
          重置密码
        </Button>,
        <Popconfirm
          key="del"
          title={`删掉「${account.name}」？`}
          description="骑手将无法再用这个名字登录（世界里的身体不受影响）。"
          okText="删号"
          okButtonProps={{ danger: true }}
          onConfirm={() => void remove()}
        >
          <Button type="link" size="small" danger disabled={busy}>
            删号
          </Button>
        </Popconfirm>,
      ]}
    >
      <List.Item.Meta
        title={
          <Space size={8}>
            <span>{account.name}</span>
            <OnlineTag online={online} />
          </Space>
        }
        description={`建号 ${fmtTime(account.created_at)} · 改密 ${fmtTime(account.updated_at)}`}
      />
    </List.Item>
  );
}

function OnlineTag({ online }: { online: boolean | null }) {
  if (online === true) return <Tag color="green">在场</Tag>;
  if (online === false) return <Tag>离线</Tag>;
  return <Tag>未登录</Tag>;
}

function CopyButton({ text, label = "复制" }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <Button
      size="small"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setDone(true);
          setTimeout(() => setDone(false), 1500);
        } catch {
          // 非安全上下文（局域网 http）下 clipboard 可能不可用，退回手选。
        }
      }}
    >
      {done ? "已复制" : label}
    </Button>
  );
}

function Heading({ children, inline }: { children: React.ReactNode; inline?: boolean }) {
  return (
    <Typography.Title level={5} style={{ margin: inline ? 0 : "24px 0 8px" }}>
      {children}
    </Typography.Title>
  );
}

/** SVG 文本 → data URL。去掉 XML 声明：data URL 里不需要它。 */
function qrDataUrl(svg: string): string {
  const body = svg.replace(/^\s*<\?xml[^>]*\?>\s*/, "");
  return `data:image/svg+xml;utf8,${encodeURIComponent(body)}`;
}

/**
 * 账号在不在场，以**当前快照**为准——HTTP 名单是打开面板那一刻的，
 * 而骑手上线 / 掉线每秒都在变。快照里找不到就退回 HTTP 读到的那份。
 */
function liveOnline(a: Account, riders: RiderBrief[] | undefined): boolean | null {
  const r = riders?.find((x) => x.kind === "human" && x.name === a.name);
  if (r) return r.online;
  return a.online ?? null;
}

function isLoopback(host: string): boolean {
  const h = host.split(":")[0];
  return h === "localhost" || h.startsWith("127.") || h === "0.0.0.0" || h === "::1";
}

function fmtTime(unixSecs: number): string {
  if (!unixSecs) return "—";
  return new Date(unixSecs * 1000).toLocaleString();
}

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

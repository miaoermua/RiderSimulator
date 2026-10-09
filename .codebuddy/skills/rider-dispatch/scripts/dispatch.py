#!/usr/bin/env python3
"""RiderSimulator 远程派单小工具。

只用标准库，方便塞进任何环境里跑。

    dispatch.py state                       # 大厅 + 在线骑手，人看得懂的摘要
    dispatch.py state --json                # 原样打印 /api/state
    dispatch.py assign --order 42 --rider r3 --reason "顺路"
    dispatch.py assign --json '{"assignments":[{"rider_id":3,"order_id":42}]}'
    dispatch.py assign --json -             # 从 stdin 读
    dispatch.py config --json '{"claim_window_secs":5}'
    dispatch.py online 2 --off
    dispatch.py online 2

环境变量：
    RIDER_SIM_BASE           默认 http://127.0.0.1:8080
    RIDER_SIM_CONTROL_TOKEN  必需，除非服务端开了 RIDER_SIM_ALLOW_CONTROL

退出码：0 全部成功；1 有业务失败；2 用法/网络/鉴权错误。
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("RIDER_SIM_BASE", "http://127.0.0.1:8080").rstrip("/")
TOKEN = os.environ.get("RIDER_SIM_CONTROL_TOKEN", "").strip()


def die(msg: str, code: int = 2) -> "None":
    print(f"错误：{msg}", file=sys.stderr)
    sys.exit(code)


def call(method: str, path: str, body: dict | None = None) -> tuple[int, object]:
    """发一个请求，返回 (HTTP 状态码, 解析后的 JSON)。"""
    url = BASE + path
    data = None if body is None else json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("accept", "application/json")
    if data is not None:
        req.add_header("content-type", "application/json")
    if TOKEN:
        req.add_header("authorization", f"Bearer {TOKEN}")

    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            raw = resp.read().decode("utf-8")
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, raw
    except urllib.error.URLError as e:
        die(f"连不上 {url}：{e.reason}")


def show_error(status: int, payload: object) -> None:
    if isinstance(payload, dict) and "error" in payload:
        err = payload["error"]
        print(f"HTTP {status} {err.get('code')}：{err.get('message')}", file=sys.stderr)
    else:
        print(f"HTTP {status}：{payload}", file=sys.stderr)


def parse_json_arg(text: str) -> object:
    if text == "-":
        text = sys.stdin.read()
    try:
        return json.loads(text)
    except json.JSONDecodeError as e:
        die(f"--json 不是合法 JSON：{e}")


def cmd_state(args: argparse.Namespace) -> int:
    status, data = call("GET", "/api/state")
    if status != 200:
        show_error(status, data)
        return 2
    if args.json:
        print(json.dumps(data, ensure_ascii=False, indent=2))
        return 0

    print(
        f"第 {data['day']} 天 {data['sim_minute'] // 60:02d}:{data['sim_minute'] % 60:02d}"
        f"  {data['weather_label']}  抢单窗口 {data['claim_window_ms'] // 1000}s"
        f"  在线 {sum(1 for r in data['riders'] if r['online'])}/{len(data['riders'])}"
    )

    hall = data["hall"]
    print(f"\n大厅待分配 {len(hall)} 单（超过 claim_deadline_min 就没了）：")
    if not hall:
        print("  （空）")
    for o in hall:
        margin = o["claim_deadline_min"] - data["elapsed_min"]
        print(
            f"  #{o['id']:<4} {o['tier']:<8} {o['distance_m'] / 1000:.1f}km"
            f"  到手 ¥{o['fee']['takehome_cents'] / 100:.2f}"
            f"  限时 {o['promised_minutes']}min  剩 {margin}min  {o['merchant_name']}"
        )

    print("\n骑手：")
    for r in data["riders"]:
        state = "在线" if r["online"] else "离线"
        flags = []
        if r["injured"]:
            flags.append("伤")
        if r["frozen"]:
            flags.append("封")
        held = ",".join(f"#{i}" for i in r["held"]) or "空"
        print(
            f"  {r['key']:<4} {r['name']:<8} {r['kind']:<9} {state}"
            f"  背{r['held'].__len__()}（{held}）"
            f"  评分 {r['rating']:.1f}  {'/'.join(flags)}"
        )
    return 0


def report_dispatch(data: object) -> int:
    applied, failed = data["applied"], data["failed"]
    for r in data["results"]:
        if r["ok"]:
            print(
                f"  派单成功 #{r['order_id']} → {r.get('rider_key')}"
                f"  预计 {r.get('planned_minutes')}min  理由：{r.get('reason')}"
            )
        else:
            print(
                f"  派单失败 #{r['order_id']} → {r.get('rider_key')}"
                f"  {r['code']}：{r['message']}",
                file=sys.stderr,
            )
    print(f"成功 {applied} 条，失败 {failed} 条")
    return 0 if failed == 0 else 1


def cmd_assign(args: argparse.Namespace) -> int:
    if args.json:
        body = parse_json_arg(args.json)
    elif args.order is not None and args.rider is not None:
        body = {
            "rider_id": args.rider,
            "order_id": args.order,
            "reason": args.reason,
        }
    else:
        die("要么给 --json，要么同时给 --order 和 --rider")

    status, data = call("POST", "/api/dispatch", body)
    if status != 200:
        show_error(status, data)
        return 1
    return report_dispatch(data)


def cmd_config(args: argparse.Namespace) -> int:
    body = parse_json_arg(args.json)
    status, data = call("POST", "/api/config", body)
    if status != 200:
        show_error(status, data)
        return 1
    print(json.dumps(data, ensure_ascii=False, indent=2))
    return 0


def cmd_online(args: argparse.Namespace) -> int:
    status, data = call("POST", f"/api/riders/{args.rider_id}/online", {"online": not args.off})
    if status != 200:
        show_error(status, data)
        return 1
    print(f"{data.get('rider_id')} 现在{'离线' if args.off else '在线'}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description="RiderSimulator 远程派单工具")
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("state", help="看大厅与骑手")
    s.add_argument("--json", action="store_true", help="原样打印接口返回")
    s.set_defaults(func=cmd_state)

    a = sub.add_parser("assign", help="派单")
    a.add_argument("--order", type=int, help="订单 id")
    a.add_argument("--rider", help="骑手 id，3 或 r3 都行")
    a.add_argument("--reason", help="派单理由，会进控制台决策记录")
    a.add_argument("--json", help="直接给接口的请求体；- 表示从 stdin 读")
    a.set_defaults(func=cmd_assign)

    c = sub.add_parser("config", help="改控制端开关")
    c.add_argument("--json", required=True, help="稀疏补丁，如 '{\"claim_window_secs\":5}'")
    c.set_defaults(func=cmd_config)

    o = sub.add_parser("online", help="让骑手上线/下线")
    o.add_argument("rider_id", type=int)
    o.add_argument("--off", action="store_true", help="改成下线，默认上线")
    o.set_defaults(func=cmd_online)

    return p


def main() -> int:
    if not TOKEN:
        print(
            "提示：未设置 RIDER_SIM_CONTROL_TOKEN，只有服务端开了 "
            "RIDER_SIM_ALLOW_CONTROL 时才能用。",
            file=sys.stderr,
        )
    args = build_parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())

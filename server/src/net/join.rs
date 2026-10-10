//! 骑手端「接入」：手机该连哪儿、二维码里放什么。
//!
//! 烽火台建完号要做两件事——告诉骑手**连哪个地址**，给一张能扫的二维码。
//! 这两件事都在这儿。
//!
//! ## 地址是这一环最容易翻车的地方
//!
//! 服务端默认绑 `127.0.0.1:8080`，而回环地址到了手机上指的是**手机自己**。
//! 照着 `127.0.0.1` 生成二维码，骑手扫完必然连不上，报错还特别像「服务端没开」。
//! 所以 [`reachable_hosts`] 会顺手把本机在局域网里的 IPv4 探测出来。
//!
//! ## 二维码里放明文密码
//!
//! ```text
//! ridersim://login?host=192.168.1.7&port=8080&name=%E5%BC%A0%E4%B8%89&password=…
//! ```
//!
//! 明文密码直接躺在码里，是**有意的取舍**（2026-10-11 定）：二维码能截图、
//! 能打印、能贴桌上，扫码即登录，不依赖服务端状态、不依赖票据过期时间。
//!
//! 代价要认：投屏或截图等于把密码给出去了，而且**发出去的码收不回来**——
//! 想作废只能去烽火台重置该骑手的密码（旧码当场失效，因为密码变了）。

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// 登录链接的 scheme。客户端解析的就是它。
pub const LOGIN_SCHEME: &str = "ridersim";

/// 手机上要填的那四样东西。**也是二维码的全部内容。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinInfo {
    /// 手机可达的主机（IPv4 或域名，不带端口）。
    pub host: String,
    pub port: u16,
    /// 骑手名字（= 账号名）。
    pub name: String,
    /// 明文密码。见模块头的取舍说明。
    pub password: String,
}

impl JoinInfo {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        name: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            name: name.into(),
            password: password.into(),
        }
    }

    /// 客户端真正去连的地址。手填路径与扫码路径最终都归到它。
    pub fn ws_url(&self) -> String {
        format!("ws://{}:{}/ws", self.host, self.port)
    }

    /// 二维码内容。手填路径要输的三样，全在里面。
    pub fn login_uri(&self) -> String {
        format!(
            "{LOGIN_SCHEME}://login?host={}&port={}&name={}&password={}",
            percent_encode(&self.host),
            self.port,
            percent_encode(&self.name),
            percent_encode(&self.password),
        )
    }

    /// 二维码图，SVG 文本。控制台直接塞进 DOM，不用引前端二维码库。
    pub fn qr_svg(&self) -> Result<String, String> {
        let code = qrcode::QrCode::new(self.login_uri().as_bytes())
            .map_err(|e| format!("内容编不成二维码：{e}"))?;
        Ok(code
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(240, 240)
            .build())
    }
}

/// 手机**真的能连上**的候选地址，主推的排第一。
///
/// 顺序：管理员显式指定的 → 监听地址（非未指定、非回环时）→ 探测到的局域网 IP
/// → 回环兜底（只保证列表不空，手机连不上，控制台应当据此提示）。
pub fn reachable_hosts(bound: SocketAddr, configured: Option<&str>) -> Vec<String> {
    let port = bound.port();
    let mut out: Vec<String> = Vec::new();

    if let Some(c) = configured.map(str::trim).filter(|c| !c.is_empty()) {
        push_unique(&mut out, with_port(c, port));
    }

    let ip = bound.ip();
    if !ip.is_unspecified() && !ip.is_loopback() {
        push_unique(&mut out, format!("{ip}:{port}"));
    }

    if let Some(lan) = lan_ipv4() {
        push_unique(&mut out, format!("{lan}:{port}"));
    }

    if out.is_empty() {
        push_unique(&mut out, format!("127.0.0.1:{port}"));
    }
    out
}

fn push_unique(out: &mut Vec<String>, host: String) {
    if !out.contains(&host) {
        out.push(host);
    }
}

/// 给一个可能是「纯主机」的配置补上端口。已经是 `主机:端口` 就原样返回。
///
/// IPv6 字面量（`[::1]:8080`）不在考虑范围内——烽火台给骑手的地址是
/// 「局域网 IPv4 或域名」，真要上 IPv6 得连方括号一起重新想。
fn with_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:{port}")
    }
}

/// 探测本机在局域网里的 IPv4。
///
/// 手法是「UDP connect + 读 local_addr」：**不发任何包**，只是让内核按默认路由
/// 挑一条出口，于是 `local_addr` 就是本机在那个方向上的地址。比枚举网卡便宜，
/// 也不用引平台相关的依赖。
///
/// 离线、没有默认路由、或者探到的地址不像是「别人能连上的」时返回 `None`。
pub fn lan_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    // 这里连的是一个「必定有路由」的公网地址，只为问内核要出口，不产生流量。
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if is_usable_lan_ipv4(v4) => Some(v4),
        _ => None,
    }
}

/// 这个 IPv4 有没有可能被**别人的手机**连上。
///
/// 要过滤掉的东西不是洁癖，是实测踩到的：开着代理 / VPN 的开发机（Clash 之类
/// 用 fake-ip 接管流量）默认路由指向虚拟网卡，探出来的是 `198.18.x.x` —— 那是
/// RFC 2544 的基准测试段，骑手的手机永远连不上。把它当成本机局域网地址生成
/// 二维码，扫出来就是「连接超时」。
fn is_usable_lan_ipv4(ip: Ipv4Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_link_local() || ip.is_multicast() {
        return false;
    }
    if ip == Ipv4Addr::BROADCAST {
        return false;
    }
    // 198.18.0.0/15：RFC 2544 基准测试段，被 fake-ip 类代理大量占用
    let [a, b, ..] = ip.octets();
    !(a == 198 && (b == 18 || b == 19))
}

/// 百分号编码。只放过 RFC 3986 的 unreserved 字符。
///
/// 手写而不是引 `percent-encoding`：这里只需要「编码」这一个方向，
/// 且规则短到能一眼看完——骑手名是中文，必须编。
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join() -> JoinInfo {
        JoinInfo::new("192.168.1.7", 8080, "张三", "abc123")
    }

    #[test]
    fn login_uri_carries_everything_the_form_needs() {
        let uri = join().login_uri();
        assert!(uri.starts_with("ridersim://login?"), "{uri}");
        // 中文名字必须编码，否则二维码扫出来是乱码
        assert!(uri.contains("name=%E5%BC%A0%E4%B8%89"), "{uri}");
        assert!(uri.contains("host=192.168.1.7"), "{uri}");
        assert!(uri.contains("port=8080"), "{uri}");
        assert!(uri.contains("password=abc123"), "{uri}");
    }

    #[test]
    fn ws_url_matches_the_protocol_path() {
        assert_eq!(join().ws_url(), "ws://192.168.1.7:8080/ws");
    }

    #[test]
    fn percent_encode_escapes_what_would_break_the_query_string() {
        assert_eq!(percent_encode("abc-._~09"), "abc-._~09");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(
            percent_encode("p@ss/w:rd?&=#"),
            "p%40ss%2Fw%3Ard%3F%26%3D%23"
        );
        assert_eq!(percent_encode("张三"), "%E5%BC%A0%E4%B8%89");
        assert_eq!(percent_encode(""), "");
    }

    #[test]
    fn qr_svg_is_real_svg() {
        let svg = join().qr_svg().unwrap();
        assert!(
            svg.starts_with("<?xml") || svg.trim_start().starts_with("<svg"),
            "{svg}"
        );
        assert!(svg.contains("<svg"), "应该是一段 SVG：{svg}");
        // 一张二维码的 SVG 不可能只有几十字节
        assert!(svg.len() > 500, "SVG 太小了，八成没画出来：{}", svg.len());
    }

    #[test]
    fn a_configured_host_wins_and_gets_the_port_appended() {
        let bound: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let hosts = reachable_hosts(bound, Some("game.example.com"));
        assert_eq!(hosts[0], "game.example.com:8080");
        // 端口已经在配置里给过就原样用
        let hosts = reachable_hosts(bound, Some("10.0.0.9:9000"));
        assert_eq!(hosts[0], "10.0.0.9:9000");
    }

    #[test]
    fn a_bound_lan_address_is_offered_even_without_probing() {
        let bound: SocketAddr = "192.168.1.7:8080".parse().unwrap();
        let hosts = reachable_hosts(bound, None);
        assert_eq!(hosts[0], "192.168.1.7:8080", "监听在局域网上就该直接推它");
    }

    #[test]
    fn loopback_only_deployments_still_get_a_non_empty_list() {
        // 线上不会这么部署，但本机裸跑就是这个样子——列表不能是空的，
        // 否则控制台会渲染出一个没有地址的二维码。
        let bound: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let hosts = reachable_hosts(bound, None);
        assert!(!hosts.is_empty());
        if lan_ipv4().is_none() {
            // 探不到局域网地址时，回环是唯一的兜底
            assert_eq!(hosts, ["127.0.0.1:8080"]);
        }
    }

    #[test]
    fn the_probe_never_offers_an_address_a_phone_cannot_reach() {
        // 探不到就是没探到；探到了就必须是一个「别人可能连上」的地址。
        // 这条挡的是代理/VPN 的 fake-ip 段（实测开发机上探到过 198.18.0.1）。
        let Some(ip) = lan_ipv4() else {
            return;
        };
        assert!(is_usable_lan_ipv4(ip), "探到了不该推给骑手的地址：{ip}");
        assert!(!ip.is_loopback() && !ip.is_link_local());
    }

    #[test]
    fn reserved_ranges_are_rejected() {
        for bad in [
            "198.18.0.1",
            "198.19.255.254",
            "127.0.0.1",
            "0.0.0.0",
            "169.254.1.1",
        ] {
            let ip: Ipv4Addr = bad.parse().unwrap();
            assert!(!is_usable_lan_ipv4(ip), "{bad} 不该被当成局域网地址");
        }
        for good in ["192.168.1.7", "10.0.0.9", "172.16.3.4", "100.101.102.103"] {
            let ip: Ipv4Addr = good.parse().unwrap();
            assert!(is_usable_lan_ipv4(ip), "{good} 应该是可用的");
        }
    }

    #[test]
    fn unspecified_bind_never_shows_up_as_an_address() {
        // 0.0.0.0 对骑手毫无意义，绝不能出现在二维码里
        let bound: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let hosts = reachable_hosts(bound, None);
        assert!(!hosts.iter().any(|h| h.starts_with("0.0.0.0")), "{hosts:?}");
    }

    #[test]
    fn duplicates_are_dropped() {
        let bound: SocketAddr = "192.168.1.7:8080".parse().unwrap();
        let hosts = reachable_hosts(bound, Some("192.168.1.7:8080"));
        let n = hosts
            .iter()
            .filter(|h| h.as_str() == "192.168.1.7:8080")
            .count();
        assert_eq!(n, 1, "{hosts:?}");
    }
}

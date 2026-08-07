//! 可选联网能力：仅用于「生成作品名」（Qwen 视觉）。
//!
//! 修图参数一律由 `retouch_core` 的纯算法模块（`auto` / `reference`）计算，
//! 不再经任何文本 / 视觉模型生成参数——那一路数值回归不准、烧 token、且
//! 曾导致过曝毁图，已在 v0.2 砍掉。本模块只保留无害、按需联网的命名能力：
//! 点了「生成作品名」才联网，不点则零网络、零 token。

use base64::Engine;
use serde_json::Value;

const QWEN_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions";

/// Qwen 视觉模型客户端：为成品图写投稿卡片文案（作品名 + 点评）。
pub struct QwenClient {
    api_key: String,
    model: String,
}

impl QwenClient {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "qwen3-vl-flash".to_string(),
        }
    }

    /// 从 `DASHSCOPE_API_KEY` 环境变量构建；未设置则返回 None。
    pub fn from_env() -> Option<Self> {
        std::env::var("DASHSCOPE_API_KEY").ok().map(Self::new)
    }

    /// 看成品缩略图，写投稿卡片文案：
    /// `{"title", "title_en", "comment", "comment_en"}`。
    pub fn review(
        &self,
        thumb_b64: &str,
        metrics_json: &str,
        process_summary: &str,
    ) -> Result<Value, String> {
        let sys = "你是图片编辑，为这张已修好的照片写投稿卡片文案。结合下方「修图流程」与客观指标，用自然、随性、真诚的口吻写点评。\
                   只输出 JSON：{\"title\":\"中文作品名(≤8字,有意境)\",\"title_en\":\"English title\",\
                   \"comment\":\"中文点评(≤65字,自然口语投稿风;可点到修了什么/画面好在哪/还能怎么更好,不列条目)\",\
                   \"comment_en\":\"English review(≤30 words, natural tone)\"}";
        let user = format!(
            "本次修图流程: {}\n成品客观指标: {}\n请先理解画面再命名并点评（只输出 JSON）。",
            process_summary, metrics_json
        );
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": sys},
                {"role": "user", "content": [
                    {"type": "text", "text": user},
                    {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", thumb_b64)}}
                ]}
            ],
            "max_tokens": 600
        });
        // ===== 追踪日志（排障用，不影响逻辑）=====
        let klen = self.api_key.trim().len();
        let kmask = if klen == 0 {
            "(empty)".to_string()
        } else {
            format!("{}…(len={})", &self.api_key.trim()[..klen.min(4)], klen)
        };
        eprintln!(
            "[qwen] review: model={} url={} key={}",
            self.model, QWEN_URL, kmask
        );
        eprintln!(
            "[qwen] payload: thumb_b64.len={} metrics.len={} summary.len={}",
            thumb_b64.len(),
            metrics_json.len(),
            process_summary.len()
        );

        // ===== 智能选路（借鉴 arlink2 netprobe：先探测、再选路、失败换路）=====
        // 1. TCP 预探测直连 dashscope:443 与代理端口是否存活（各 ~2s 超时）；
        // 2. 按存活情况排出尝试顺序：直连通→直连优先；直连不通且代理活→代理优先；
        //    代理配置了但端口死（如 VPN 已关）→ 自动忽略，不撞死在死代理上；
        // 3. 第一条路失败自动换第二条重试。DashScope 是国内服务，无 VPN 直连即通。
        let proxy = detect_proxy().filter(|p| {
            let alive = probe_proxy_alive(p);
            if !alive {
                eprintln!("[qwen] 代理 {} 端口无响应（VPN 已关？），忽略之", p);
            }
            alive
        });
        let direct_ok = probe_tcp("dashscope.aliyuncs.com:443", 2500);
        eprintln!(
            "[qwen] 选路探测: 直连={} 代理={}",
            if direct_ok { "通" } else { "不通" },
            proxy.as_deref().unwrap_or("(无)")
        );
        let mut routes: Vec<Option<String>> = Vec::new();
        if direct_ok {
            routes.push(None); // 直连优先
            if let Some(p) = &proxy {
                routes.push(Some(p.clone()));
            }
        } else {
            if let Some(p) = &proxy {
                routes.push(Some(p.clone())); // 代理优先
            }
            routes.push(None); // 直连兜底（探测可能误判）
        }

        let mut last_err = String::new();
        let mut v: Option<Value> = None;
        for route in &routes {
            let tag = route.as_deref().unwrap_or("直连");
            eprintln!("[qwen] -> POST via {} (timeout=25s)", tag);
            match self.post_once(&body, route.as_deref()) {
                Ok(val) => {
                    v = Some(val);
                    break;
                }
                Err(e) => {
                    eprintln!("[qwen] via {} 失败: {}", tag, e);
                    last_err = e;
                }
            }
        }
        let v = v.ok_or_else(|| format!("Qwen 请求失败（已尝试全部线路）: {}", last_err))?;
        let text = v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("{}");
        let obj: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        Ok(obj)
    }

    /// 用指定线路发一次请求（`proxy=None` 直连）。
    fn post_once(&self, body: &Value, proxy: Option<&str>) -> Result<Value, String> {
        let mut builder = ureq::builder().timeout(std::time::Duration::from_secs(25));
        // 手动接 native-tls：ureq 2.12 的 native-tls feature 不会自动接线，
        // 尤其走代理的 HTTPS CONNECT 隧道必须显式提供 TLS 连接器，否则报 no TLS backend。
        let tls = native_tls::TlsConnector::new().map_err(|e| format!("TLS 初始化失败: {}", e))?;
        builder = builder.tls_connector(std::sync::Arc::new(tls));
        if let Some(p) = proxy {
            let proxy_obj = ureq::Proxy::new(p).map_err(|e| format!("代理地址无效: {}", e))?;
            builder = builder.proxy(proxy_obj);
        }
        let agent = builder.build();
        let resp = agent
            .post(QWEN_URL)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body.clone());
        match &resp {
            Ok(r) => eprintln!("[qwen] <- HTTP status={}", r.status()),
            Err(e) => eprintln!("[qwen] <- HTTP ERROR: {:?}", e),
        }
        let resp = resp.map_err(|e| format!("请求失败: {}", e))?;
        resp.into_json().map_err(|e| format!("响应解析失败: {}", e))
    }
}

/// TCP 预探测：`addr` 形如 `host:port`，在 `timeout_ms` 内能建立连接即视为通。
fn probe_tcp(addr: &str, timeout_ms: u64) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let timeout = std::time::Duration::from_millis(timeout_ms);
    let Ok(mut addrs) = addr.to_socket_addrs() else {
        return false; // DNS 解析失败（离线/被劫持）
    };
    addrs.any(|sa| TcpStream::connect_timeout(&sa, timeout).is_ok())
}

/// 探测代理端口是否存活（解析 `http://host:port` 后 TCP 连接）。
fn probe_proxy_alive(proxy_url: &str) -> bool {
    let hostport = proxy_url
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_start_matches("socks5://")
        .trim_end_matches('/');
    if hostport.is_empty() {
        return false;
    }
    probe_tcp(hostport, 1500)
}

/// 探测 HTTPS 代理地址（返回 None 表示直连）。
/// 优先级：进程 env（HTTPS_PROXY / HTTP_PROXY / 小写）> 登录 shell best-effort。
/// Finder 双击启动的 GUI 不继承终端 env，但用户在「作品名设置」手动填的代理会被
/// 写进进程 env，这里就能读到；若都没有则直连。
fn detect_proxy() -> Option<String> {
    for var in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    // best-effort：从登录 shell 取（GUI 从 Finder 启动、且未手动配置代理时尝试）
    for sh in ["/bin/zsh", "/bin/bash"] {
        if let Ok(out) = std::process::Command::new(sh)
            .args(["-lc", "echo -n \"$HTTPS_PROXY\""])
            .output()
        {
            if let Ok(s) = String::from_utf8(out.stdout) {
                let s = s.trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// 把图片缩放到最长边 `max_side` 以内并返回 base64 JPEG（质量 82）。
/// 用于把小缩略图传给视觉模型——单次调用 token 成本约 ¥0.0001。
pub fn thumb_b64(path: &std::path::Path, max_side: u32) -> Result<String, String> {
    use image::{imageops::FilterType, ImageFormat};
    let img = image::open(path).map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    let img = if w.max(h) > max_side {
        let r = max_side as f32 / w.max(h) as f32;
        img.resize(
            (w as f32 * r) as u32,
            (h as f32 * r) as u32,
            FilterType::Lanczos3,
        )
    } else {
        img
    };
    let mut buf = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut buf);
        img.write_to(&mut cursor, ImageFormat::Jpeg)
            .map_err(|e| e.to_string())?;
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}

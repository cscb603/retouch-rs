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
        let resp = ureq::post(QWEN_URL)
            .timeout(std::time::Duration::from_secs(25))
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| format!("Qwen 请求失败: {}", e))?;
        let v: Value = resp
            .into_json()
            .map_err(|e| format!("Qwen 响应解析失败: {}", e))?;
        let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("{}");
        let obj: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        Ok(obj)
    }
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

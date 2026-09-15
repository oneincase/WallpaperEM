//! Steam 个人资料抓取：SteamID64 → 昵称 + 头像（无需 Web API Key）。
//!
//! 首选 `?xml=1` 资料页（结构稳定、字段直给；私密资料也照常返回昵称/头像）；
//! 失败回退 HTML 页解析 <title> 与头像 img。都失败返回 None，由调用方回退
//! 展示（通常是裸 SteamID64）。结果由 db 层缓存（author_profiles），本模块
//! 只做抓取与解析，纯函数可测。

use super::SteamClient;
use crate::util::decode_entities;

/// 抓取 (昵称, 头像 URL)。头像可能取不到，此时为空串；昵称取不到才算失败。
pub async fn fetch_persona(client: &SteamClient, steamid64: &str) -> Option<(String, String)> {
    if !valid_steamid64(steamid64) {
        return None;
    }
    let url = format!("https://steamcommunity.com/profiles/{steamid64}?xml=1");
    if let Ok(resp) = client.get(&url).await {
        if let Ok(text) = resp.text().await {
            if let Some(found) = parse_profile_xml(&text) {
                return Some(found);
            }
        }
    }
    // XML 接口偶发抽风/被限流 → HTML 页兜底
    let url = format!("https://steamcommunity.com/profiles/{steamid64}");
    let resp = client.get(&url).await.ok()?;
    let html = resp.text().await.ok()?;
    parse_profile_html(&html)
}

/// SteamID64 是纯数字串（17 位上下）；拼 URL 前挡掉畸形输入
fn valid_steamid64(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

/// 解析 `?xml=1` 资料页。昵称在 `<steamID><![CDATA[...]]></steamID>`
/// （注意不是 steamID64）；头像优先 avatarMedium，缺省退 avatarIcon/avatarFull。
/// 资料不存在时 Steam 返回 `<response><error>...</error></response>`，解析自然落空。
fn parse_profile_xml(xml: &str) -> Option<(String, String)> {
    let name = xml_field(xml, "steamID")?;
    let avatar = xml_field(xml, "avatarMedium")
        .or_else(|| xml_field(xml, "avatarIcon"))
        .or_else(|| xml_field(xml, "avatarFull"))
        .unwrap_or_default();
    Some((name, avatar))
}

/// 取 XML 字段值：兼容 CDATA 与裸文本两种写法
fn xml_field(xml: &str, tag: &str) -> Option<String> {
    let cdata = format!(r#"(?s)<{tag}>\s*<!\[CDATA\[(.*?)\]\]>\s*</{tag}>"#);
    let plain = format!(r#"(?s)<{tag}>\s*(.*?)\s*</{tag}>"#);
    for pat in [cdata, plain] {
        if let Some(m) = regex::Regex::new(&pat).ok()?.captures(xml) {
            let v = m.get(1)?.as_str().trim();
            if !v.is_empty() {
                return Some(decode_entities(v));
            }
        }
    }
    None
}

/// HTML 兜底：`<title>Steam Community :: 昵称</title>` +
/// 头像块 `playerAvatarAutoSizeInner` 里的 img
fn parse_profile_html(html: &str) -> Option<(String, String)> {
    let title_re = regex::Regex::new(r"(?s)<title>\s*Steam Community ::\s*(.*?)\s*</title>").ok()?;
    let name = title_re
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| decode_entities(m.as_str().trim()))
        .filter(|s| !s.is_empty() && !s.contains("Error"))?;
    let avatar_re =
        regex::Regex::new(r#"playerAvatarAutoSizeInner[^>]*>\s*<img[^>]+src="([^"]+)""#).ok()?;
    let avatar = avatar_re
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    Some((name, avatar))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_parses_persona_and_avatar() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<profile>
	<steamID64>76561198314933366</steamID64>
	<steamID><![CDATA[abi toads]]></steamID>
	<onlineState>offline</onlineState>
	<privacyState>public</privacyState>
	<avatarIcon><![CDATA[https://avatars.fastly.steamstatic.com/aaa_medium.jpg]]></avatarIcon>
	<avatarMedium><![CDATA[https://avatars.fastly.steamstatic.com/bbb_medium.jpg]]></avatarMedium>
	<avatarFull><![CDATA[https://avatars.fastly.steamstatic.com/ccc_full.jpg]]></avatarFull>
</profile>"#;
        let (name, avatar) = parse_profile_xml(xml).unwrap();
        assert_eq!(name, "abi toads");
        assert_eq!(avatar, "https://avatars.fastly.steamstatic.com/bbb_medium.jpg");
    }

    #[test]
    fn xml_steamid_does_not_match_steamid64() {
        // <steamID> 的正则不能误吞 <steamID64> 的数字
        let xml = r#"<profile><steamID64>76561198314933366</steamID64><steamID><![CDATA[name]]></steamID></profile>"#;
        assert_eq!(xml_field(xml, "steamID").as_deref(), Some("name"));
    }

    #[test]
    fn xml_missing_profile_returns_none() {
        let xml = r#"<?xml version="1.0"?><response><error>The specified profile could not be found.</error></response>"#;
        assert!(parse_profile_xml(xml).is_none());
    }

    #[test]
    fn xml_plain_text_field_accepted() {
        // 个别字段可能不是 CDATA；昵称带 & 时 HTML 实体要解码
        let xml = r#"<profile><steamID>Tom &amp; Jerry</steamID></profile>"#;
        assert_eq!(xml_field(xml, "steamID").as_deref(), Some("Tom & Jerry"));
    }

    #[test]
    fn html_fallback_parses_title_and_avatar() {
        let html = r#"<html><head><title>Steam Community :: abi toads</title></head>
<body><div class="playerAvatarAutoSizeInner"><img src="https://avatars.steamstatic.com/x_full.jpg"></div></body></html>"#;
        let (name, avatar) = parse_profile_html(html).unwrap();
        assert_eq!(name, "abi toads");
        assert_eq!(avatar, "https://avatars.steamstatic.com/x_full.jpg");
    }

    #[test]
    fn rejects_malformed_steamid() {
        assert!(!valid_steamid64(""));
        assert!(!valid_steamid64("76561198314933366/../../"));
        assert!(!valid_steamid64("not-a-number"));
        assert!(valid_steamid64("76561198314933366"));
    }
}

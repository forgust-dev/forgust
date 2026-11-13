//! Webhooks: signed with HMAC-SHA256, delivered by workers with retries, and
//! refused for private, loopback and link-local addresses (SSRF).

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use forgust_db::projects::Project;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

use crate::error::{ApiError, ApiResult};
use crate::jobs::DeliverWebhook;
use crate::state::AppState;

pub const EVENTS: &[&str] = &["push", "merge_request", "note", "release", "pipeline", "build"];

/// GitLab's `X-Gitlab-Event` for an event, for receivers written for GitLab.
pub fn gitlab_event(event: &str) -> &'static str {
    match event {
        "push" => "Push Hook",
        "merge_request" => "Merge Request Hook",
        "note" => "Note Hook",
        "release" => "Release Hook",
        "pipeline" => "Pipeline Hook",
        "build" => "Job Hook",
        _ => "System Hook",
    }
}
pub const MAX_ATTEMPTS: i32 = 5;

/// Queue a delivery to every hook (project and ancestor groups) that wants `event`.
pub async fn emit(state: &AppState, p: &Project, event: &str, payload: Value) {
    let r: anyhow::Result<()> = async {
        let hooks = forgust_db::webhooks::for_event(&state.db, p.id, event).await?;
        if hooks.is_empty() {
            return Ok(());
        }
        let mut tx = state.db.begin().await?;
        for h in hooks {
            let id = forgust_db::webhooks::create_delivery(&mut *tx, h.id, event, &payload).await?;
            state.jobs.enqueue(&mut tx, DeliverWebhook { delivery_id: id }.request()).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(project = p.id, event, error = %e, "webhook not queued");
    }
}

pub fn validate_url(raw: &str) -> ApiResult<url::Url> {
    let u = url::Url::parse(raw).map_err(|_| ApiError::bad("invalid URL"))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() || !u.username().is_empty() {
        return Err(ApiError::bad("webhook URLs must be http(s) without credentials"));
    }
    Ok(u)
}

fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // carrier-grade NAT 100.64/10
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal(IpAddr::V4(v4));
            }
            let s = v6.segments();
            let embedded = |hi: u16, lo: u16| std::net::Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
            // IPv4 carried inside IPv6: NAT64 (64:ff9b::/96 and the local-use
            // 64:ff9b:1::/48), 6to4 (2002::/16) and Teredo (2001::/32, client
            // address inverted) reach the IPv4 address they embed
            if s[0] == 0x64 && s[1] == 0xff9b && (s[2..6] == [0, 0, 0, 0] || s[2] == 1) {
                return s[2] == 1 || is_internal(IpAddr::V4(embedded(s[6], s[7])));
            }
            if s[0] == 0x2002 {
                return is_internal(IpAddr::V4(embedded(s[1], s[2])));
            }
            if s[0] == 0x2001 && s[1] == 0 {
                return is_internal(IpAddr::V4(embedded(!s[6], !s[7])));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link local
                || (s[0] & 0xffc0) == 0xfec0 // site local (deprecated)
                || (s[0] == 0x100 && s[1..4] == [0, 0, 0]) // discard-only 100::/64
                || (s[0] == 0x2001 && s[1] == 0xdb8) // documentation
                || s[..6] == [0, 0, 0, 0, 0, 0] // IPv4-compatible ::a.b.c.d (deprecated)
        }
    }
}

/// Resolve the host and pin the request to the checked addresses, so DNS cannot
/// change between the check and the connection.
pub(crate) async fn safe_addrs(u: &url::Url, allow_local: bool) -> Result<Vec<SocketAddr>, String> {
    let host = u.host_str().ok_or("no host")?;
    let port = u.port_or_known_default().ok_or("no port")?;
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await.map_err(|e| format!("DNS: {e}"))?.collect();
    if addrs.is_empty() {
        return Err("host did not resolve".into());
    }
    if !allow_local && addrs.iter().any(|a| is_internal(a.ip())) {
        return Err("refused: the host resolves to an internal address".into());
    }
    Ok(addrs)
}

/// GET a public http(s) URL with the same SSRF guards as webhooks (no internal
/// addresses, pinned DNS, no redirects), reading at most `max_bytes`.
pub async fn safe_get(state: &AppState, raw: &str, max_bytes: usize) -> Result<String, String> {
    let url = validate_url(raw).map_err(|e| e.to_string())?;
    let addrs = safe_addrs(&url, state.cfg.features.webhooks_allow_local).await?;
    let host = url.host_str().unwrap_or_default().to_string();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .resolve_to_addrs(&host, &addrs)
        .build()
        .map_err(|e| e.to_string())?;
    let resp =
        client.get(url).header("user-agent", concat!("Forgust/", env!("CARGO_PKG_VERSION"))).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if body.len() + chunk.len() > max_bytes {
            return Err(format!("larger than {max_bytes} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| "not UTF-8".to_string())
}

/// An image for the Markdown image proxy: http(s) only, internal addresses
/// refused at every hop (up to 3 redirects), `image/*` only, at most `max_bytes`.
pub async fn fetch_image(state: &AppState, raw: &str, max_bytes: usize) -> Result<(String, Vec<u8>), String> {
    let mut url = validate_url(raw).map_err(|e| e.to_string())?;
    for _ in 0..4 {
        // its own switch, not `webhooks_allow_local`: that one trusts
        // Maintainers' webhooks, not every comment's images
        let addrs = safe_addrs(&url, state.cfg.features.image_proxy_allow_local).await?;
        let host = url.host_str().unwrap_or_default().to_string();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .resolve_to_addrs(&host, &addrs)
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(url.clone())
            .header("user-agent", concat!("Forgust-image-proxy/", env!("CARGO_PKG_VERSION")))
            .header("accept", "image/*")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status().is_redirection() {
            let loc = resp.headers().get("location").and_then(|v| v.to_str().ok()).ok_or("redirect without a location")?;
            url = validate_url(url.join(loc).map_err(|e| e.to_string())?.as_str()).map_err(|e| e.to_string())?;
            continue;
        }
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status().as_u16()));
        }
        let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
        let ctype = ctype.split(';').next().unwrap_or("").trim().to_string();
        if !ctype.starts_with("image/") {
            return Err(format!("not an image ({ctype})"));
        }
        if resp.content_length().is_some_and(|n| n as usize > max_bytes) {
            return Err(format!("larger than {max_bytes} bytes"));
        }
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            if body.len() + chunk.len() > max_bytes {
                return Err(format!("larger than {max_bytes} bytes"));
            }
            body.extend_from_slice(&chunk);
        }
        return Ok((ctype, body));
    }
    Err("too many redirects".into())
}

pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(secret.as_bytes()).expect("any key length works");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Deliver one queued delivery (worker). Returns Err to make the job retry.
pub async fn deliver(state: &AppState, delivery_id: i64) -> anyhow::Result<()> {
    let Some(d) = forgust_db::webhooks::pending_delivery(&state.db, delivery_id).await? else { return Ok(()) };
    let (event, payload) = (d.event, d.payload);
    let Some(hook) = forgust_db::webhooks::find(&state.db, d.webhook_id).await? else { return Ok(()) };
    // a chat app gets one message, and nothing for what would only be noise
    let body = if hook.format == "generic" {
        serde_json::to_vec(&payload)?
    } else {
        match super::chat::render(&hook.format, &event, &payload) {
            Some(msg) => serde_json::to_vec(&msg)?,
            None => {
                forgust_db::webhooks::record_attempt(
                    &state.db,
                    delivery_id,
                    true,
                    false,
                    None,
                    Some("not sent: nothing to tell the channel"),
                    0,
                )
                .await?;
                return Ok(());
            }
        }
    };
    let start = Instant::now();
    let attempt: Result<u16, String> = async {
        let url = validate_url(&hook.url).map_err(|e| e.to_string())?;
        let addrs = safe_addrs(&url, state.cfg.features.webhooks_allow_local).await?;
        let host = url.host_str().unwrap_or_default().to_string();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .resolve_to_addrs(&host, &addrs)
            .build()
            .map_err(|e| e.to_string())?;
        let mut req = client
            .post(url)
            .header("content-type", "application/json")
            .header("user-agent", concat!("Forgust-Hookshot/", env!("CARGO_PKG_VERSION")))
            .header("x-forgust-event", &event)
            .header("x-gitlab-event", gitlab_event(&event))
            .header("x-forgust-delivery", delivery_id.to_string());
        if let Some(sealed) = &hook.secret_enc {
            let secret = String::from_utf8(state.secrets.decrypt(sealed).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            req = req.header("x-forgust-signature", sign(&secret, &body));
        }
        let resp = req.body(body.clone()).send().await.map_err(|e| e.to_string())?;
        Ok(resp.status().as_u16())
    }
    .await;
    let ms = start.elapsed().as_millis().min(i32::MAX as u128) as i32;
    let last = d.attempts + 1 >= MAX_ATTEMPTS;
    match attempt {
        Ok(code) if (200..300).contains(&code) => {
            forgust_db::webhooks::record_attempt(&state.db, delivery_id, true, false, Some(code as i32), None, ms).await?;
            metrics::counter!("forgust_webhook_deliveries_total", "result" => "delivered").increment(1);
            Ok(())
        }
        Ok(code) => {
            forgust_db::webhooks::record_attempt(&state.db, delivery_id, false, last, Some(code as i32), Some("non-2xx response"), ms)
                .await?;
            metrics::counter!("forgust_webhook_deliveries_total", "result" => "failed").increment(1);
            if last { Ok(()) } else { anyhow::bail!("HTTP {code}") }
        }
        Err(e) => {
            let refused = e.starts_with("refused");
            forgust_db::webhooks::record_attempt(&state.db, delivery_id, false, last || refused, None, Some(&e), ms).await?;
            metrics::counter!("forgust_webhook_deliveries_total", "result" => "failed").increment(1);
            if last || refused { Ok(()) } else { anyhow::bail!(e) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_unchanged() {
        // receivers check this header: the value from the previous hmac version
        assert_eq!(sign("whsec", b"{\"a\":1}"), "sha256=19373ef0e8c66f206e49d251e4402291615bbce6ef00922e94a47a213fb76ee1");
    }

    #[test]
    fn internal_addresses_are_refused() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_internal(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "93.184.216.34", "2606:4700:4700::1111", "64:ff9b::5db8:d822", "2002:5db8:d822::1"] {
            assert!(!is_internal(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn ipv4_inside_ipv6_is_checked_too() {
        for ip in [
            "64:ff9b::7f00:1",                      // NAT64 of 127.0.0.1
            "64:ff9b::a9fe:a9fe",                   // NAT64 of 169.254.169.254
            "64:ff9b:1::1",                         // local-use NAT64
            "2002:7f00:1::1",                       // 6to4 of 127.0.0.1
            "2002:a00:1::",                         // 6to4 of 10.0.0.1
            "2001:0:4136:e378:8000:63bf:80ff:fffe", // Teredo of 127.0.0.1
            "::7f00:1",                             // IPv4-compatible 127.0.0.1
            "2001:db8::1",
            "fec0::1",
        ] {
            assert!(is_internal(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn signature_is_hmac_sha256() {
        // RFC 4231 test case 2
        assert_eq!(
            sign("Jefe", b"what do ya want for nothing?"),
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert!(validate_url("ftp://x").is_err());
        assert!(validate_url("https://user:pw@example.com").is_err());
        assert!(validate_url("https://example.com/hook").is_ok());
    }
}

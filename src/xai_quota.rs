//! Grok's billing wire format, checked against CC Switch's subscription_grok.rs.
//! The endpoint has no published protobuf schema; fail closed on unknown usage.
use crate::credentials::Secret;
use std::time::Duration;

pub(crate) const ENDPOINT: &str =
    "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";

#[derive(Clone, Debug, PartialEq)]
pub struct Quota {
    pub remaining_percent: f32,
    pub resets_at: Option<i64>,
    pub queried_at: i64,
    pub period_label: &'static str,
}

pub(crate) async fn fetch(
    client: &reqwest::Client,
    endpoint: &str,
    token: &Secret,
) -> Result<Quota, String> {
    let mut response = client
        .post(endpoint)
        .bearer_auth(token.expose())
        .header("Origin", "https://grok.com")
        .header("Referer", "https://grok.com/?_s=usage")
        .header("Accept", "*/*")
        .header("Content-Type", "application/grpc-web+proto")
        .header("x-grpc-web", "1")
        .header("x-user-agent", "connect-es/2.1.1")
        .body(vec![0u8; 5])
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| "额度查询连接失败，请稍后刷新")?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Err("额度查询授权被拒绝，请重新登录 Grok".into());
    }
    if !status.is_success() {
        return Err(format!("额度服务暂不可用（HTTP {}）", status.as_u16()));
    }
    check_status(
        response
            .headers()
            .get("grpc-status")
            .and_then(|v| v.to_str().ok()),
    )?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "额度响应读取失败")? {
        if bytes.len() + chunk.len() > 64 * 1024 {
            return Err("额度响应过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    parse(&bytes, chrono::Utc::now().timestamp())
}

fn check_status(status: Option<&str>) -> Result<(), String> {
    match status.map(str::trim) {
        None | Some("0") => Ok(()),
        Some("16") => Err("额度查询授权被拒绝，请重新登录 Grok".into()),
        Some("7") => Err("当前账号没有额度查询权限".into()),
        Some("9") => Err("当前账号的账单额度暂不可查询".into()),
        // Never surface server messages: they may echo credentials or personal data.
        _ => Err("额度服务暂不可用，请稍后刷新".into()),
    }
}

#[derive(Clone, Copy)]
enum Number {
    Integer(u64),
    Float(f32),
}

fn varint(bytes: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..=63).step_by(7) {
        let (&byte, rest) = bytes.split_first()?;
        *bytes = rest;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Option<&'a [u8]> {
    if count > bytes.len() {
        return None;
    }
    let (part, rest) = bytes.split_at(count);
    *bytes = rest;
    Some(part)
}

fn scan(mut bytes: &[u8], path: &[u64]) -> Option<Vec<(Vec<u64>, Number)>> {
    let mut fields = Vec::new();
    while !bytes.is_empty() {
        let key = varint(&mut bytes)?;
        if key >> 3 == 0 || key >> 3 > 0x1fffffff {
            return None;
        }
        let mut path = path.to_vec();
        path.push(key >> 3);
        match key & 7 {
            0 => fields.push((path, Number::Integer(varint(&mut bytes)?))),
            1 => {
                take(&mut bytes, 8)?;
            }
            2 => {
                let length = usize::try_from(varint(&mut bytes)?).ok()?;
                let nested = take(&mut bytes, length)?;
                if path.len() < 5 {
                    fields.extend(scan(nested, &path).unwrap_or_default());
                }
            }
            5 => fields.push((
                path,
                Number::Float(f32::from_le_bytes(take(&mut bytes, 4)?.try_into().ok()?)),
            )),
            _ => return None,
        }
    }
    Some(fields)
}

fn parse(data: &[u8], now: i64) -> Result<Quota, String> {
    let invalid = || "未能识别官方额度数据，请稍后刷新".to_owned();
    let mut payloads = Vec::new();
    if matches!(data.first(), Some(0 | 0x80)) {
        let mut frames = data;
        while !frames.is_empty() {
            let header = take(&mut frames, 5).ok_or_else(invalid)?;
            let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
            let payload = take(&mut frames, length).ok_or_else(invalid)?;
            match header[0] {
                0 => payloads.push(payload),
                0x80 => {
                    let trailer = std::str::from_utf8(payload).map_err(|_| invalid())?;
                    for line in trailer.lines() {
                        if let Some((name, value)) = line.split_once(':')
                            && name.trim().eq_ignore_ascii_case("grpc-status")
                        {
                            check_status(Some(value))?;
                        }
                    }
                }
                _ => return Err(invalid()),
            }
        }
    } else {
        payloads.push(data);
    }
    let mut fields = Vec::new();
    for payload in payloads {
        fields.extend(scan(payload, &[]).ok_or_else(invalid)?);
    }
    let resets_at = fields
        .iter()
        .filter_map(|(path, number)| {
            if let Number::Integer(value) = number
                && (1_700_000_000..=2_100_000_000).contains(value)
                && *value as i64 > now
            {
                Some((path.as_slice() != [1, 5, 1], *value as i64))
            } else {
                None
            }
        })
        .min()
        .map(|(_, value)| value);
    let percent = fields
        .iter()
        .filter_map(|(path, number)| {
            if let Number::Float(value) = number
                && path.last() == Some(&1)
                && value.is_finite()
                && (0.0..=100.0).contains(value)
            {
                Some((path.len(), *value))
            } else {
                None
            }
        })
        .min_by_key(|(depth, _)| *depth)
        .map(|(_, value)| value);
    let period_marker = fields.iter().any(|(path, number)| {
        matches!(number, Number::Integer(_))
            && (path.starts_with(&[1, 6])
                || (path.as_slice() == [1, 8, 1] && matches!(number, Number::Integer(1 | 2))))
    });
    // Proto3 omits a zero float. A reset + period marker distinguishes zero from unknown.
    let used = percent
        .or_else(|| {
            (resets_at.is_some()
                && period_marker
                && !fields.iter().any(|(_, v)| matches!(v, Number::Float(_))))
            .then_some(0.0)
        })
        .ok_or_else(invalid)?;
    // This label is an estimate, as in CC Switch; the actual reset comes from billing.
    let days = resets_at.map(|reset| ((reset - now) as f64 / 86400.0).round() as i64);
    let period_label = match days {
        Some(4..=12) => "每周额度",
        Some(20..=45) => "每月额度",
        _ => "订阅额度",
    };
    Ok(Quota {
        remaining_percent: 100.0 - used,
        resets_at,
        queried_at: now,
        period_label,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: i64 = 1_791_520_000;
    fn integer(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        while value >= 128 {
            bytes.push(value as u8 | 0x80);
            value >>= 7;
        }
        bytes.push(value as u8);
        bytes
    }
    fn message(field: u8, bytes: &[u8]) -> Vec<u8> {
        [
            vec![field << 3 | 2],
            integer(bytes.len() as u64),
            bytes.to_vec(),
        ]
        .concat()
    }
    fn frame(flags: u8, bytes: &[u8]) -> Vec<u8> {
        [
            vec![flags],
            (bytes.len() as u32).to_be_bytes().to_vec(),
            bytes.to_vec(),
        ]
        .concat()
    }
    fn billing(percent: Option<f32>, reset: i64) -> Vec<u8> {
        let mut bytes = percent
            .map(|v| [vec![13], v.to_le_bytes().to_vec()].concat())
            .unwrap_or_default();
        bytes.extend(message(5, &[vec![8], integer(reset as u64)].concat()));
        bytes.extend(message(6, &[8, 3]));
        message(1, &bytes)
    }
    #[test]
    fn billing_formats_zero_and_invalid_percentages() {
        for (used, days, label) in [
            (2.0, 7, "每周额度"),
            (37.5, 30, "每月额度"),
            (100.0, 1, "订阅额度"),
        ] {
            let payload = billing(Some(used), NOW + days * 86400);
            for bytes in [&payload, &frame(0, &payload)] {
                let quota = parse(bytes, NOW).unwrap();
                assert_eq!(quota.remaining_percent, 100.0 - used);
                assert_eq!(quota.period_label, label);
                assert_eq!(quota.resets_at, Some(NOW + days * 86400));
            }
        }
        assert_eq!(
            parse(&billing(None, NOW + 86400), NOW)
                .unwrap()
                .remaining_percent,
            100.0
        );
        for value in [f32::NAN, f32::INFINITY, -1.0, 101.0] {
            assert!(parse(&billing(Some(value), NOW + 86400), NOW).is_err());
        }
        for bytes in [
            vec![],
            vec![0, 0, 0, 0, 255],
            vec![10, 255],
            vec![8, 42],
            vec![0x80; 12],
        ] {
            assert!(parse(&bytes, NOW).is_err());
        }
    }
    #[test]
    fn ignores_deeper_percent_and_rejects_trailer_errors_without_echoing_them() {
        let payload = message(
            1,
            &[message(2, &[13, 0, 0, 198, 66]), vec![13, 0, 0, 200, 65]].concat(),
        );
        assert_eq!(parse(&payload, NOW).unwrap().remaining_percent, 75.0);
        for status in ["7", "9", "16", "14"] {
            let bytes = [
                frame(0, &billing(Some(2.0), NOW + 86400)),
                frame(
                    0x80,
                    format!("grpc-status: {status}\r\ngrpc-message: private-token").as_bytes(),
                ),
            ]
            .concat();
            let error = parse(&bytes, NOW).unwrap_err();
            assert!(!error.contains("private-token"));
        }
    }
    #[tokio::test]
    async fn request_is_bounded_and_never_follows_redirects_or_displays_server_secrets() {
        use axum::{
            Router,
            body::Bytes,
            http::{HeaderMap, StatusCode},
            routing::post,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route(
                "/billing",
                post(|headers: HeaderMap, body: Bytes| async move {
                    assert_eq!(headers["authorization"], "Bearer synthetic-access");
                    assert_eq!(headers["content-type"], "application/grpc-web+proto");
                    assert_eq!(headers["origin"], "https://grok.com");
                    assert_eq!(body.as_ref(), &[0; 5]);
                    frame(
                        0,
                        &billing(Some(2.0), chrono::Utc::now().timestamp() + 7 * 86400),
                    )
                }),
            )
            .route(
                "/denied",
                post(|| async { (StatusCode::UNAUTHORIZED, "private-token") }),
            )
            .route("/oversized", post(|| async { vec![0; 65537] }))
            .route(
                "/redirect",
                post(|| async { (StatusCode::FOUND, [("location", "https://evil.invalid")]) }),
            )
            .route(
                "/grpc-error",
                post(|| async {
                    (
                        [("grpc-status", "16"), ("grpc-message", "private-token")],
                        "",
                    )
                }),
            );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let token = Secret::new("synthetic-access".into());
        assert_eq!(
            fetch(&client, &format!("{origin}/billing"), &token)
                .await
                .unwrap()
                .remaining_percent,
            98.0
        );
        for path in ["denied", "oversized", "redirect", "grpc-error"] {
            let error = fetch(&client, &format!("{origin}/{path}"), &token)
                .await
                .unwrap_err();
            assert!(!error.contains("private-token"));
            assert!(!error.contains("synthetic-access"));
        }
        task.abort();
    }
}

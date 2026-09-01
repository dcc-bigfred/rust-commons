//! Same-origin HTTP reverse proxy to BigFred (no WebSocket upgrades).

use crate::error::{Error, Result};
use crate::wire::IMPERSONATE_HEADER;

/// Request headers forwarded upstream. Everything else (cookies, host,
/// connection controls) is dropped on purpose.
const FORWARDED_REQUEST_HEADERS: [&str; 3] = ["authorization", "content-type", "accept"];

/// Response headers copied back to the caller.
const FORWARDED_RESPONSE_HEADERS: [&str; 2] = ["content-type", "cache-control"];

/// Headers the host should copy from the inbound HTTP request.
pub fn forwarded_request_header_names() -> impl Iterator<Item = &'static str> {
    FORWARDED_REQUEST_HEADERS
        .iter()
        .copied()
        .chain(std::iter::once(IMPERSONATE_HEADER))
}

/// One proxied response. The host converts this into its HTTP framework type.
#[derive(Debug)]
pub struct ForwardResponse {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
}

/// Forwards `method path?query` to `api_base`. `headers` must already be
/// filtered to the allowlist (see [`forwarded_request_header_names`]).
pub async fn forward(
    client: &reqwest::Client,
    api_base: &str,
    method: &str,
    path: &str,
    query: Option<&str>,
    headers: &[(&str, &[u8])],
    body: &[u8],
) -> Result<ForwardResponse> {
    let mut url = format!("{api_base}{path}");
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }

    let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut out = client.request(method, &url);
    for (name, value) in headers {
        out = out.header(*name, *value);
    }
    if !body.is_empty() {
        out = out.body(body.to_vec());
    }

    let res = out
        .send()
        .await
        .map_err(|err| Error::ProxyUnreachable(err.to_string()))?;

    let status = res.status().as_u16();
    let mut headers = Vec::new();
    for name in FORWARDED_RESPONSE_HEADERS {
        if let Some(value) = res.headers().get(name) {
            headers.push((name.to_string(), value.as_bytes().to_vec()));
        }
    }
    let body = res
        .bytes()
        .await
        .map_err(|err| Error::ProxyReadFailed(err.to_string()))?
        .to_vec();

    Ok(ForwardResponse {
        status,
        headers,
        body,
    })
}

//! Reading `ClientInfo` off a connection request.
//!
//! Ports `ParseClientInfo` and `AugmentClientInfo` in `pkg/service/utils.go`.
//! Everything the client states about itself arrives as query parameters on the
//! signal WebSocket; the server fills in the address it sees.
//!
//! # Not done here
//!
//! The Go server also parses the `User-Agent` with `uap-go` to fill `browser`,
//! `os` and `device_model` when the client did not send them, which feeds the
//! client-configuration rules in `lk-room`. That needs the ua-parser regex
//! corpus vendored and kept in sync; until it is, a client that sends those
//! parameters (every current SDK does) is unaffected, and one that does not
//! matches no codec rule rather than the wrong one.

use std::collections::BTreeMap;

use axum::http::HeaderMap;
use lk_proto::livekit::{ClientInfo, client_info};

/// Builds a `ClientInfo` from the query parameters and headers of a connection
/// request.
#[must_use]
pub fn parse_client_info(
    params: &BTreeMap<String, String>,
    headers: &HeaderMap,
    peer_address: Option<&str>,
) -> ClientInfo {
    let get = |key: &str| params.get(key).map(String::as_str).unwrap_or_default();

    let mut info = ClientInfo {
        protocol: get("protocol").parse().unwrap_or_default(),
        client_protocol: get("client_protocol").parse().unwrap_or_default(),
        sdk: sdk_from_param(get("sdk")) as i32,
        version: get("version").to_owned(),
        os: get("os").to_owned(),
        os_version: get("os_version").to_owned(),
        browser: get("browser").to_owned(),
        browser_version: get("browser_version").to_owned(),
        device_model: get("device_model").to_owned(),
        network: get("network").to_owned(),
        ..ClientInfo::default()
    };

    let capabilities = get("capabilities");
    if !capabilities.is_empty() {
        info.capabilities = capabilities
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .filter_map(client_info::Capability::from_str_name)
            .map(|capability| capability as i32)
            .collect();
    }

    augment_client_info(&mut info, headers, peer_address);
    info
}

/// Fills in what the server knows about a client that the client did not say.
pub fn augment_client_info(info: &mut ClientInfo, headers: &HeaderMap, peer_address: Option<&str>) {
    info.address = client_ip(headers, peer_address);
}

/// The client's address, preferring the proxy headers in the Go server's order.
///
/// Cloudflare's header is checked first because it is the outermost proxy when
/// it is present; `X-Forwarded-For` is next, then `X-Real-IP`, then the socket.
#[must_use]
pub fn client_ip(headers: &HeaderMap, peer_address: Option<&str>) -> String {
    for header in ["cf-connecting-ip", "x-forwarded-for", "x-real-ip"] {
        if let Some(value) = headers.get(header).and_then(|v| v.to_str().ok())
            && !value.is_empty()
        {
            return value.to_owned();
        }
    }
    // the socket address without its port, as net.SplitHostPort gives
    peer_address
        .map(|address| match address.rsplit_once(':') {
            Some((host, _)) => host.trim_matches(['[', ']']).to_owned(),
            None => address.to_owned(),
        })
        .unwrap_or_default()
}

/// The `sdk` parameter's enum value. `ios` and `swift` are the same SDK.
fn sdk_from_param(sdk: &str) -> client_info::Sdk {
    match sdk {
        "js" => client_info::Sdk::Js,
        "ios" | "swift" => client_info::Sdk::Swift,
        "android" => client_info::Sdk::Android,
        "flutter" => client_info::Sdk::Flutter,
        "go" => client_info::Sdk::Go,
        "unity" => client_info::Sdk::Unity,
        "reactnative" => client_info::Sdk::ReactNative,
        "rust" => client_info::Sdk::Rust,
        "python" => client_info::Sdk::Python,
        "cpp" => client_info::Sdk::Cpp,
        "unityweb" => client_info::Sdk::UnityWeb,
        "node" => client_info::Sdk::Node,
        "esp32" => client_info::Sdk::Esp32,
        _ => client_info::Sdk::Unknown,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn reads_the_parameters_the_sdks_send() {
        let info = parse_client_info(
            &params(&[
                ("protocol", "17"),
                ("client_protocol", "16"),
                ("sdk", "js"),
                ("version", "2.7.0"),
                ("os", "macos"),
                ("os_version", "15.0"),
                ("browser", "safari"),
                ("browser_version", "18.4"),
                ("device_model", "MacBookPro18,1"),
                ("network", "wifi"),
                ("capabilities", "CAP_PACKET_TRAILER, ,nonsense"),
            ]),
            &HeaderMap::new(),
            Some("192.0.2.9:51234"),
        );

        assert_eq!(info.protocol, 17);
        assert_eq!(info.client_protocol, 16);
        assert_eq!(info.sdk, client_info::Sdk::Js as i32);
        assert_eq!(info.version, "2.7.0");
        assert_eq!(info.browser, "safari");
        assert_eq!(info.browser_version, "18.4");
        assert_eq!(info.device_model, "MacBookPro18,1");
        assert_eq!(info.network, "wifi");
        // unknown capability names are dropped rather than failing the join
        assert_eq!(
            info.capabilities,
            vec![client_info::Capability::CapPacketTrailer as i32]
        );
        assert_eq!(info.address, "192.0.2.9");
    }

    #[test]
    fn ios_and_swift_are_the_same_sdk() {
        assert_eq!(sdk_from_param("ios"), client_info::Sdk::Swift);
        assert_eq!(sdk_from_param("swift"), client_info::Sdk::Swift);
        assert_eq!(sdk_from_param("brand-new"), client_info::Sdk::Unknown);
    }

    #[test]
    fn proxy_headers_win_over_the_socket_address() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.7"));
        assert_eq!(client_ip(&headers, Some("10.0.0.1:443")), "198.51.100.7");

        headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.5"));
        assert_eq!(client_ip(&headers, Some("10.0.0.1:443")), "203.0.113.5");
    }

    #[test]
    fn an_ipv6_socket_address_loses_its_port_and_brackets() {
        assert_eq!(
            client_ip(&HeaderMap::new(), Some("[2001:db8::1]:51234")),
            "2001:db8::1"
        );
        assert_eq!(client_ip(&HeaderMap::new(), None), "");
    }
}

//! 内嵌的 Web UI 产物（`crates/astrcode-webui/www`），挂在站点根路径下。
//!
//! `www/wasm/` 是 wasm 产物、不入库，因此本模块不假设它一定存在，只会在缺失时把请求
//! 落到 JSON 404。根路径的派发由公开扩展路由的兜底触发（扩展路由优先），见
//! `routes/extensions.rs`。
//!
//! 产物只在 `embed-webui` feature 下编译进二进制；桌面 App（astrcode-gui）用
//! `default-features = false` 排除它，`serve` 退化为恒 `None`，由扩展路由兜底 404。

use axum::{
    extract::OriginalUri,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

/// 桌面 App 独立前的 Web UI 挂载点，保留为 308 跳转，让旧链接不失效。
const LEGACY_MOUNT_PREFIX: &str = "/app";

/// `/app` 及其子路径跳到根路径的等价位置。路由只把该前缀下的路径交给本函数。
pub(in crate::http) async fn redirect_legacy_mount(OriginalUri(uri): OriginalUri) -> Response {
    let rest = uri
        .path()
        .strip_prefix(LEGACY_MOUNT_PREFIX)
        .unwrap_or_default();
    let mut location = if rest.is_empty() {
        "/".to_owned()
    } else {
        rest.to_owned()
    };
    if let Some(query) = uri.query() {
        location.push('?');
        location.push_str(query);
    }
    (
        StatusCode::PERMANENT_REDIRECT,
        [(header::LOCATION.as_str(), location)],
    )
        .into_response()
}

#[cfg(feature = "embed-webui")]
mod embedded {
    use std::fmt::Write as _;

    use axum::{
        body::Body,
        http::{StatusCode, header},
        response::{IntoResponse, Response},
    };

    const INDEX_HTML_PATH: &str = "index.html";
    const WASM_GLUE_PATH: &str = "wasm/astrcode_webui.js";
    /// wasm 产物文件名不由内容决定，强缓存会在重新构建后给出旧 UI，因此每次都回源校验。
    const NO_CACHE: &str = "no-cache";

    /// 跨源隔离头：gpui 的 web 平台依赖 `SharedArrayBuffer`，而它只在跨源隔离的文档里可用。
    const CROSS_ORIGIN_OPENER_POLICY: &str = "cross-origin-opener-policy";
    const CROSS_ORIGIN_EMBEDDER_POLICY: &str = "cross-origin-embedder-policy";
    const SAME_ORIGIN: &str = "same-origin";
    const REQUIRE_CORP: &str = "require-corp";

    #[derive(rust_embed::RustEmbed)]
    #[folder = "../../crates/astrcode-webui/www"]
    pub(super) struct WebUiAssets;

    /// 内嵌产物里是否带上了 wasm 胶水脚本，供启动时告警。
    pub(in crate::http) fn has_wasm_artifact() -> bool {
        WebUiAssets::get(WASM_GLUE_PATH).is_some()
    }

    /// 返回内嵌的 Web UI 产物；路径没有对应条目时返回 `None`，由调用方决定兜底语义。
    ///
    /// `rust_embed` 的 `get` 是对内嵌键的精确查找，不经过文件系统，因此不存在路径穿越。
    pub(in crate::http) fn serve(path: &str, if_none_match: Option<&str>) -> Option<Response> {
        let entry = normalize(path)?;
        let file = WebUiAssets::get(entry)?;
        let etag = etag(file.metadata.sha256_hash());
        let content_type = file.metadata.mimetype();

        if if_none_match.is_some_and(|value| etag_matches(value, &etag)) {
            return Some(asset_response(
                StatusCode::NOT_MODIFIED,
                content_type,
                &etag,
                Body::empty(),
            ));
        }

        Some(asset_response(
            StatusCode::OK,
            content_type,
            &etag,
            Body::from(file.data.into_owned()),
        ))
    }

    fn asset_response(status: StatusCode, content_type: &str, etag: &str, body: Body) -> Response {
        (
            status,
            [
                (header::CONTENT_TYPE.as_str(), content_type.to_owned()),
                (header::CACHE_CONTROL.as_str(), NO_CACHE.to_owned()),
                (header::ETAG.as_str(), etag.to_owned()),
                (CROSS_ORIGIN_OPENER_POLICY, SAME_ORIGIN.to_owned()),
                (CROSS_ORIGIN_EMBEDDER_POLICY, REQUIRE_CORP.to_owned()),
            ],
            body,
        )
            .into_response()
    }

    /// `/` 映射到 `index.html`；其余路径去掉前导 `/` 后按内嵌键查找。
    fn normalize(path: &str) -> Option<&str> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Some(INDEX_HTML_PATH);
        }
        // 目录请求（`/wasm/`）没有对应的内嵌条目。
        if path.ends_with('/') {
            return None;
        }
        Some(path)
    }

    /// 强 ETag：`rust_embed` 编译期已为每个文件算好 SHA256，直接借用即可。
    fn etag(hash: [u8; 32]) -> String {
        let mut hex = String::with_capacity(64);
        for byte in hash {
            let _ = write!(hex, "{byte:02x}");
        }
        format!("\"{hex}\"")
    }

    /// `If-None-Match` 允许逗号分隔的多个候选与 `*`，弱比较前缀 `W/` 也要认。
    fn etag_matches(if_none_match: &str, etag: &str) -> bool {
        if_none_match.split(',').any(|candidate| {
            let candidate = candidate.trim();
            candidate == "*" || candidate == etag || candidate.strip_prefix("W/") == Some(etag)
        })
    }

    #[cfg(test)]
    mod tests {
        use axum::http::StatusCode;

        use super::{etag_matches, normalize, serve};

        #[test]
        fn the_site_root_maps_to_index_and_directory_paths_are_rejected() {
            assert_eq!(normalize("/"), Some("index.html"));
            assert_eq!(normalize(""), Some("index.html"));
            assert_eq!(
                normalize("/wasm/astrcode_webui.js"),
                Some("wasm/astrcode_webui.js")
            );
            assert_eq!(normalize("/wasm/"), None);
        }

        #[test]
        fn if_none_match_accepts_lists_wildcards_and_weak_comparison() {
            assert!(etag_matches("\"abc\"", "\"abc\""));
            assert!(etag_matches("\"x\", \"abc\"", "\"abc\""));
            assert!(etag_matches("*", "\"abc\""));
            assert!(etag_matches("W/\"abc\"", "\"abc\""));
            assert!(!etag_matches("\"other\"", "\"abc\""));
        }

        #[test]
        fn issued_etag_revalidates_and_isolation_headers_are_present() {
            let response = serve("/", None).expect("index.html 随产物目录内嵌");
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["cross-origin-opener-policy"],
                "same-origin"
            );
            assert_eq!(
                response.headers()["cross-origin-embedder-policy"],
                "require-corp"
            );
        }
    }
}

#[cfg(feature = "embed-webui")]
pub(in crate::http) use embedded::{has_wasm_artifact, serve};

#[cfg(not(feature = "embed-webui"))]
mod embedded {
    use axum::response::Response;

    /// 桌面 App 构建不内嵌 Web UI 产物是有意为之：恒 `None`，由扩展路由兜底 JSON 404。
    pub(in crate::http) fn serve(_path: &str, _if_none_match: Option<&str>) -> Option<Response> {
        None
    }
}

#[cfg(not(feature = "embed-webui"))]
pub(in crate::http) use embedded::serve;

//! astrcode 桌面应用（GPUI Kit）。
//!
//! 宿主只做三件事：在进程内引导 `astrcode-server`（绑 `127.0.0.1:0`）、给 gpui 装一个原生
//! HTTP 客户端、把 server 地址与工作目录注入共享 UI 层。视图与状态在 `astrcode-ui`，
//! 与 Web 宿主共用（ADR 0001）。

use std::sync::Arc;

use astrcode_ui::{api::Api, views::shell::Shell};
use gpui_kit::{AppContext as _, WindowBounds, WindowOptions, px, size};
use reqwest_client::ReqwestClient;

pub mod server;

/// 启动桌面应用。
pub fn run() {
    let _log_guard = astrcode_log::init();

    // `local_server` 必须活到本函数返回：它所在的 server 线程一停，进程内 server 就跟着没了。
    //
    // 引导 server 也必须在 gpui 事件循环之外完成：进入 `application().run` 之后
    // 这个线程就交给了窗口循环，不能再阻塞在 server 启动上。
    let local_server = match server::start() {
        Ok(server) => server,
        Err(error) => {
            tracing::error!(%error, "无法引导本地 server");
            eprintln!("astrcode 启动失败：{error}");
            return;
        },
    };
    let base_url = local_server.base_url().to_string();

    // gpui 的默认 HTTP 客户端会让所有请求失败，原生宿主得自己装一个。它自带 tokio 运行时，
    // 所以共享 UI 层不必再接触 tokio（ADR 0001 第 5 轮）。
    let http = match ReqwestClient::user_agent("astrcode-gui") {
        Ok(client) => Arc::new(client),
        Err(error) => {
            tracing::error!(%error, "无法构造 HTTP 客户端");
            eprintln!("astrcode 启动失败：{error}");
            return;
        },
    };

    let working_dir = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_default();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .with_http_client(http)
        .run(move |cx| {
            gpui_kit::init(cx);
            // 观感由 `astrcode-ui` 决定，宿主只负责注入它：Web 宿主调的是同一个函数，
            // 两个宿主因此不会各自长成一套。
            astrcode_ui::theme::install(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1180.), px(780.)), cx)),
                ..Default::default()
            };
            let base_url = base_url.clone();
            let working_dir = working_dir.clone();
            gpui_kit::open_window(options, cx, move |window, cx| {
                let api = Api::new(base_url.clone(), cx.http_client());
                cx.new(|cx| Shell::new(api, working_dir.clone(), window, cx))
            })
            .expect("打开窗口失败");
        });
}

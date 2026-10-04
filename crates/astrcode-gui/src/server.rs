//! 进程内引导 `astrcode-server`。
//!
//! 桌面 App 与 Web UI 同构：都走本地 HTTP + SSE 消费 `astrcode-protocol`
//! （ADR 0001 第 4 轮）。差别只在 server 的来源——App 自己 `bootstrap_with` 出
//! runtime，绑 `127.0.0.1:0`（第 5 轮），因此端口在 HTTP 服务起来之前就已知。

use std::{net::SocketAddr, sync::Arc};

use astrcode_extension_sdk::transport::{TransportFeature, TransportProfile};
use astrcode_server::{
    bootstrap::{BootstrapOptions, ServerApp, bootstrap_with},
    http::run_http_server_with_listener,
};

/// 进程内 server 的入口地址。
#[derive(Debug, Clone)]
pub struct LocalServer {
    base_url: String,
}

impl LocalServer {
    /// 形如 `http://127.0.0.1:41234`，无尾斜杠。
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LocalServerError {
    #[error("启动 server 线程失败：{0}")]
    Thread(String),
    #[error("引导 server 失败：{0}")]
    Bootstrap(String),
    #[error("绑定或运行本地 HTTP 服务失败：{0}")]
    Http(String),
    #[error("本地 server 在报告端口前退出")]
    ExitedEarly,
}

/// 在独立线程上引导 server 并常驻，返回它的入口地址。
///
/// 调用方拿到地址时 HTTP 服务可能尚未开始 accept，首个请求失败时可重试。
pub fn start() -> Result<LocalServer, LocalServerError> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("astrcode-server".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(LocalServerError::Thread(error.to_string())));
                    return;
                },
            };
            runtime.block_on(async move {
                if let Err(error) = serve(ready_tx).await {
                    tracing::error!(%error, "本地 server 退出");
                }
            });
        })
        .map_err(|error| LocalServerError::Thread(error.to_string()))?;

    match ready_rx.recv() {
        Ok(Ok(server)) => Ok(server),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(LocalServerError::ExitedEarly),
    }
}

async fn serve(
    ready: std::sync::mpsc::Sender<Result<LocalServer, LocalServerError>>,
) -> Result<(), LocalServerError> {
    let server_runtime = bootstrap_with(BootstrapOptions {
        transport_profile: TransportProfile::new([TransportFeature::AuthenticatedHttp]),
        ..Default::default()
    })
    .await
    .map_err(|error| LocalServerError::Bootstrap(error.to_string()))?;

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .map_err(|error| LocalServerError::Http(error.to_string()))?;
    let addr = listener
        .local_addr()
        .map_err(|error| LocalServerError::Http(error.to_string()))?;

    let server = LocalServer {
        base_url: format!("http://{addr}"),
    };
    // 调用方已经放弃时不必再把服务跑起来。
    if ready.send(Ok(server)).is_err() {
        return Ok(());
    }

    run_http_server_with_listener(ServerApp::new(Arc::new(server_runtime)), listener)
        .await
        .map_err(|error| LocalServerError::Http(error.to_string()))
}

//! 宿主无关的 UI 层。
//!
//! 这里只放两个宿主（桌面 App 与 Web UI）都要的东西：协议客户端、会话状态机、
//! gpui-kit 视图。宿主差异——server 在哪个地址、HTTP 客户端从哪来、工作目录是什么——
//! 由宿主在启动时注入，本 crate 不读进程状态。

pub(crate) mod agent_session;
pub mod api;
pub(crate) mod ask_user;
pub(crate) mod assistant_run;
pub(crate) mod composer_config;
pub(crate) mod composer_queue;
pub mod conversation;
pub(crate) mod icons;
pub mod kanban;
pub(crate) mod metrics;
pub(crate) mod pending_ask_user;
pub mod preferences;
pub(crate) mod session_list;
pub mod settings;
pub(crate) mod slash_command;
pub mod theme;
pub(crate) mod todo_list;
pub mod tool_view;
pub mod views;

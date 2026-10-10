//! server 的 HTTP + SSE 客户端。
//!
//! 只做协议边界的读写：请求/响应 DTO 直接来自 `astrcode-protocol`，不另造镜像类型；
//! 扩展自有路由（askUser、看板）的形状归扩展所有，镜像分别见 [`crate::pending_ask_user`]
//! 与 [`crate::kanban::wire`]。
//!
//! 传输走 gpui 的 `HttpClient`：桌面宿主装 reqwest 实现，Web 宿主装 fetch 实现，
//! 本层不感知差别，也因此不再需要 tokio 运行时（ADR 0001）。

use std::{collections::VecDeque, sync::Arc};

use astrcode_protocol::{
    http::{
        ApplyProviderPresetRequest, ApplyProviderPresetResponseDto, AvailableModelDto,
        CommandCompletionRequest, CommandCompletionResponse, ConfigReloadResponseDto,
        ConfigViewResponseDto, ConversationSnapshotResponseDto, ConversationStreamEnvelopeDto,
        CreateSessionRequest, CreateSessionResponseDto, CurrentModelResponseDto,
        DeleteProjectResponseDto, ExtensionListResponseDto, ExtensionReloadResponseDto,
        FileContentResponseDto, FileDiffResponseDto, FileTreeResponseDto, ForkSessionRequest,
        GitStatusResponseDto, ModelListResponseDto, ModelTestResponseDto, PromptRequest,
        PromptSubmitResponse, ProviderCatalogResponseDto, RemoveProviderPresetRequest,
        RemoveProviderPresetResponseDto, SessionListItemDto, SessionListResponseDto,
        SetExtensionEnabledRequest, SetExtensionEnabledResponseDto, SlashCommandListResponseDto,
        ToolApprovalRequest, UiPreferencesResponseDto, UpdateActiveSelectionRequest,
        UpdateActiveSelectionResponseDto, UpdateModelOptionsRequest, UpdateModelOptionsResponseDto,
        UpdateUiPreferencesRequest,
    },
    wire::ApprovalDecisionDto,
};
use futures_util::AsyncReadExt as _;
use gpui_kit::http_client::{AsyncBody, HttpClient, Request, Response};

use crate::kanban::{BoardResponse, Card, CreateCardRequest, DirectoryListing, UpdateCardRequest};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("请求 {path} 失败：{message}")]
    Request { path: String, message: String },
    #[error("服务端返回 {status}：{body}")]
    Status { status: u16, body: String },
    #[error("{path} 的响应无法解析：{message}")]
    Decode { path: String, message: String },
    #[error("{path} 的请求体无法编码：{message}")]
    Encode { path: String, message: String },
}

/// server 的 HTTP 客户端。
#[derive(Clone)]
pub struct Api {
    base_url: String,
    http: Arc<dyn HttpClient>,
}

impl Api {
    pub fn new(base_url: impl Into<String>, http: Arc<dyn HttpClient>) -> Self {
        Self {
            base_url: base_url.into(),
            http,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionListItemDto>, ApiError> {
        let path = "/api/sessions";
        let response: SessionListResponseDto = self.get_json(path).await?;
        Ok(response.sessions)
    }

    /// 扩展清单；宿主据此判断看板入口是否可见。
    pub async fn list_extensions(
        &self,
    ) -> Result<Vec<astrcode_protocol::http::ExtensionStateDto>, ApiError> {
        let response: ExtensionListResponseDto = self.get_json("/api/extensions").await?;
        Ok(response.extensions)
    }

    /// 重载扩展注册表；返回每个装载失败的扩展与原因。
    pub async fn reload_extensions(&self) -> Result<ExtensionReloadResponseDto, ApiError> {
        self.post_json("/api/extensions/reload", &serde_json::json!({}))
            .await
    }

    /// 启用或禁用单个扩展；服务端会顺手重载注册表，返回那一轮的装载错误。
    pub async fn set_extension_enabled(
        &self,
        extension_id: &str,
        enabled: bool,
    ) -> Result<SetExtensionEnabledResponseDto, ApiError> {
        let body = SetExtensionEnabledRequest {
            extension_id: extension_id.to_owned(),
            enabled,
        };
        self.post_json("/api/extensions/set-enabled", &body).await
    }

    pub async fn create_session(&self, working_dir: &str) -> Result<String, ApiError> {
        let path = "/api/sessions";
        let body = CreateSessionRequest {
            working_dir: working_dir.to_string(),
            tool_selection: None,
        };
        let response: CreateSessionResponseDto = self.post_json(path, &body).await?;
        Ok(response.session_id)
    }

    /// 分叉出一个新会话，返回它的 id。
    ///
    /// `storage_seq` 是源会话里的持久化点，为空时从源会话末尾分叉。
    pub async fn fork_session(
        &self,
        session_id: &str,
        storage_seq: Option<u64>,
    ) -> Result<String, ApiError> {
        let path = format!("/api/sessions/{session_id}/fork");
        let body = ForkSessionRequest { storage_seq };
        let response: CreateSessionResponseDto = self.post_json(&path, &body).await?;
        Ok(response.session_id)
    }

    /// 删除一个会话；服务端连带清掉它的持久化记录。
    pub async fn delete_session(&self, session_id: &str) -> Result<(), ApiError> {
        self.delete_empty(&format!("/api/sessions/{session_id}"))
            .await
    }

    /// 删除一个工作目录下的全部会话，返回删掉的条数。
    ///
    /// 工作目录作为查询参数跨边界，字符集不受我们控制：路径里可以有空格、`#`、`&`。
    pub async fn delete_project(&self, working_dir: &str) -> Result<usize, ApiError> {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("workingDir", working_dir)
            .finish();
        let path = format!("/api/projects?{query}");
        let response: DeleteProjectResponseDto = self.delete_json(&path).await?;
        Ok(response.deleted_count)
    }

    pub async fn conversation(
        &self,
        session_id: &str,
    ) -> Result<ConversationSnapshotResponseDto, ApiError> {
        self.get_json(&format!("/api/sessions/{session_id}/conversation"))
            .await
    }

    /// 提交一条输入。
    ///
    /// 返回 `Handled` 表示它没进 turn：服务端把 `/命令` 或被接收进队列的输入就地处理掉了。
    pub async fn submit_prompt(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<PromptSubmitResponse, ApiError> {
        let body = PromptRequest {
            text: text.to_string(),
            attachments: Vec::new(),
        };
        self.post_json(&format!("/api/sessions/{session_id}/prompt"), &body)
            .await
    }

    /// 把一条输入注入当前 turn（mid-turn steer）。
    ///
    /// 与 `submit_prompt` 不同，这里要求会话确实有活跃 turn：没有时服务端回
    /// `no_active_turn`，调用方据此提示「turn 已结束」。
    pub async fn inject_message(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<PromptSubmitResponse, ApiError> {
        let body = PromptRequest {
            text: text.to_string(),
            attachments: Vec::new(),
        };
        self.post_json(&format!("/api/sessions/{session_id}/inject"), &body)
            .await
    }

    /// 会话可用的斜杠命令，以及插件注册的快捷键与状态栏项。
    pub async fn list_commands(
        &self,
        session_id: &str,
    ) -> Result<SlashCommandListResponseDto, ApiError> {
        self.get_json(&format!("/api/sessions/{session_id}/commands"))
            .await
    }

    /// 斜杠命令的参数补全候选；`cursor` 是参数内已输入部分的字符数。
    pub async fn complete_command(
        &self,
        session_id: &str,
        command: &str,
        argument: &str,
        cursor: usize,
    ) -> Result<CommandCompletionResponse, ApiError> {
        let path = format!("/api/sessions/{session_id}/commands/{command}/complete");
        let body = CommandCompletionRequest {
            argument: argument.to_string(),
            cursor: Some(cursor),
        };
        self.post_json(&path, &body).await
    }

    /// 宿主配置视图：当前选区（含权限模式）与全部 profile。
    pub async fn config(&self) -> Result<ConfigViewResponseDto, ApiError> {
        self.get_json("/api/config").await
    }

    /// Provider 预设目录：全部可一键应用的 provider spec。
    pub async fn provider_catalog(&self) -> Result<ProviderCatalogResponseDto, ApiError> {
        self.get_json("/api/config/provider-catalog").await
    }

    /// 从磁盘重载配置，返回重载后的当前选区。
    pub async fn reload_config(&self) -> Result<ConfigReloadResponseDto, ApiError> {
        self.post_json("/api/config/reload", &serde_json::json!({}))
            .await
    }

    /// 测试当前主模型的连通性。
    pub async fn test_model(&self) -> Result<ModelTestResponseDto, ApiError> {
        self.post_json("/api/models/test", &serde_json::json!({}))
            .await
    }

    /// 改写某个模型的选项（thinking 配置）。
    pub async fn update_model_options(
        &self,
        request: &UpdateModelOptionsRequest,
    ) -> Result<UpdateModelOptionsResponseDto, ApiError> {
        self.post_json("/api/config/model-options", request).await
    }

    /// 应用 provider 预设：写入或覆盖一个 profile，可选同时激活。
    pub async fn apply_provider_preset(
        &self,
        request: &ApplyProviderPresetRequest,
    ) -> Result<ApplyProviderPresetResponseDto, ApiError> {
        self.post_json("/api/config/provider-preset/apply", request)
            .await
    }

    /// 取消一个 provider 预设 profile。
    pub async fn remove_provider_preset(
        &self,
        request: &RemoveProviderPresetRequest,
    ) -> Result<RemoveProviderPresetResponseDto, ApiError> {
        self.post_json("/api/config/provider-preset/remove", request)
            .await
    }

    /// 界面偏好。
    ///
    /// 响应里的 `stored` 为假表示服务端还没有偏好，界面据此决定是否迁移旧 localStorage
    /// 键（见 [`crate::preferences::legacy_preferences_seed`]）。
    pub async fn ui_preferences(&self) -> Result<UiPreferencesResponseDto, ApiError> {
        self.get_json("/api/preferences").await
    }

    /// 整份覆盖界面偏好，返回落盘后的版本。
    pub async fn save_ui_preferences(
        &self,
        request: &UpdateUiPreferencesRequest,
    ) -> Result<UiPreferencesResponseDto, ApiError> {
        self.put_json("/api/preferences", request).await
    }

    /// 改写当前选区。
    ///
    /// 成功时服务端的身体只有 `success: true`（`routes/config.rs`），没有可用的信息，
    /// 所以这里只往外交成功与否。
    pub async fn update_active_selection(
        &self,
        request: &UpdateActiveSelectionRequest,
    ) -> Result<UpdateActiveSelectionResponseDto, ApiError> {
        self.post_json("/api/config/active-selection", request)
            .await
    }

    /// 宿主上全部可选的模型，按 profile 平铺。
    pub async fn list_models(&self) -> Result<Vec<AvailableModelDto>, ApiError> {
        let response: ModelListResponseDto = self.get_json("/api/models").await?;
        Ok(response.models)
    }

    /// 当前选中的模型。
    pub async fn current_model(&self) -> Result<CurrentModelResponseDto, ApiError> {
        self.get_json("/api/models/current").await
    }

    pub async fn abort(&self, session_id: &str) -> Result<(), ApiError> {
        self.post_empty(
            &format!("/api/sessions/{session_id}/abort"),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn resolve_approval(
        &self,
        session_id: &str,
        call_id: &str,
        decision: ApprovalDecisionDto,
    ) -> Result<(), ApiError> {
        let body = ToolApprovalRequest {
            call_id: call_id.to_string(),
            decision,
        };
        self.post_empty(&format!("/api/sessions/{session_id}/approve"), &body)
            .await
    }

    /// 作答 `askUser` 问卷；端点由扩展自己提供。
    pub async fn respond_ask_user(
        &self,
        session_id: &str,
        call_id: &str,
        answers: &[(String, String)],
    ) -> Result<(), ApiError> {
        let answers: serde_json::Map<String, serde_json::Value> = answers
            .iter()
            .map(|(question, answer)| (question.clone(), serde_json::Value::String(answer.clone())))
            .collect();
        let body = serde_json::json!({ "answers": answers });
        self.post_empty(&ask_user_url(session_id, call_id, "respond"), &body)
            .await
    }

    /// 拒绝 `askUser` 问卷：turn 会带着「用户拒绝」这一结果继续。
    pub async fn reject_ask_user(&self, session_id: &str, call_id: &str) -> Result<(), ApiError> {
        self.post_empty(
            &ask_user_url(session_id, call_id, "reject"),
            &serde_json::json!({}),
        )
        .await
    }

    /// 跨会话的待回答问卷快照。
    ///
    /// 响应体原样交出：这个形状归扩展所有（不在 `astrcode-protocol` 里），解码在
    /// [`crate::pending_ask_user`]——那里也是同一份题面的解析入口。
    pub async fn pending_ask_user_questions(&self) -> Result<serde_json::Value, ApiError> {
        self.get_json(PENDING_ASK_USER_PATH).await
    }

    /// 看板卡片清单。
    pub async fn kanban_cards(&self) -> Result<Vec<Card>, ApiError> {
        let response: BoardResponse = self.get_json(&kanban_url("/board")).await?;
        Ok(response.cards)
    }

    /// 新建卡片；省略的字段由扩展自己补默认值。
    pub async fn kanban_create_card(&self, request: &CreateCardRequest) -> Result<Card, ApiError> {
        self.post_json(&kanban_url("/cards"), request).await
    }

    /// 更新卡片；只发要改的字段，未列出的字段保持原值。
    pub async fn kanban_update_card(
        &self,
        card_id: &str,
        request: &UpdateCardRequest,
    ) -> Result<Card, ApiError> {
        self.patch_json(&kanban_url(&format!("/cards/{card_id}")), request)
            .await
    }

    /// 删除卡片。
    pub async fn kanban_delete_card(&self, card_id: &str) -> Result<(), ApiError> {
        self.delete_empty(&kanban_url(&format!("/cards/{card_id}")))
            .await
    }

    /// 列举一层子目录，供文件夹选择器使用；`path` 为空表示从服务端进程的当前目录开始。
    pub async fn kanban_list_directories(&self, path: &str) -> Result<DirectoryListing, ApiError> {
        self.post_json(
            &kanban_url("/directories"),
            &serde_json::json!({ "path": path }),
        )
        .await
    }

    /// 列举浏览根目录下的一层条目；`path` 是相对根目录的路径，空串指根目录。
    pub async fn file_tree(&self, root: &str, path: &str) -> Result<FileTreeResponseDto, ApiError> {
        self.get_json(&files_url("/tree", root, path)).await
    }

    /// 读取一个文件的正文。
    pub async fn file_content(
        &self,
        root: &str,
        path: &str,
    ) -> Result<FileContentResponseDto, ApiError> {
        self.get_json(&files_url("/content", root, path)).await
    }

    /// 取文件相对 git HEAD 的未提交改动。
    pub async fn file_diff(&self, root: &str, path: &str) -> Result<FileDiffResponseDto, ApiError> {
        self.get_json(&files_url("/diff", root, path)).await
    }

    /// 取整个工作区相对 git HEAD 的未提交改动清单。
    pub async fn worktree_status(&self, root: &str) -> Result<GitStatusResponseDto, ApiError> {
        self.get_json(&files_root_url("/status", root)).await
    }

    /// 订阅会话事件流。`cursor` 为空表示从当前快照之后开始。
    pub async fn subscribe(
        &self,
        session_id: &str,
        cursor: Option<&str>,
    ) -> Result<ConversationStream, ApiError> {
        let path = match cursor {
            Some(cursor) => format!("/api/sessions/{session_id}/stream?cursor={cursor}"),
            None => format!("/api/sessions/{session_id}/stream"),
        };
        let request = Request::builder()
            .method("GET")
            .uri(self.url(&path))
            .header("accept", "text/event-stream")
            .body(AsyncBody::empty())
            .map_err(|error| ApiError::Request {
                path: path.clone(),
                message: error.to_string(),
            })?;
        let response = self.send(request, &path).await?;
        let status = response.status();
        if !status.is_success() {
            // 非 2xx 的响应体只进报错信息，读不出来就退回空串。
            let body = read_text(&path, response).await.unwrap_or_default();
            return Err(ApiError::Status {
                status: status.as_u16(),
                body,
            });
        }
        Ok(ConversationStream {
            path,
            body: response.into_body(),
            decoder: SseDecoder::default(),
            pending: VecDeque::new(),
            finished: false,
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let response = self.send_without_body("GET", path).await?;
        decode_json(path, response).await
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, ApiError> {
        let response = self.send_json("POST", path, body).await?;
        decode_json(path, response).await
    }

    async fn put_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, ApiError> {
        let response = self.send_json("PUT", path, body).await?;
        decode_json(path, response).await
    }

    async fn patch_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, ApiError> {
        let response = self.send_json("PATCH", path, body).await?;
        decode_json(path, response).await
    }

    async fn post_empty(&self, path: &str, body: &impl serde::Serialize) -> Result<(), ApiError> {
        let response = self.send_json("POST", path, body).await?;
        require_success(path, response).await
    }

    async fn delete_empty(&self, path: &str) -> Result<(), ApiError> {
        let response = self.send_without_body("DELETE", path).await?;
        require_success(path, response).await
    }

    /// 带着响应体的 DELETE：目前只有删除项目用得上（要回删掉的条数）。
    async fn delete_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let response = self.send_without_body("DELETE", path).await?;
        decode_json(path, response).await
    }

    /// 不带请求体的方法（GET / DELETE）共用同一套构造。
    async fn send_without_body(
        &self,
        method: &str,
        path: &str,
    ) -> Result<Response<AsyncBody>, ApiError> {
        let request = Request::builder()
            .method(method)
            .uri(self.url(path))
            .body(AsyncBody::empty())
            .map_err(|error| ApiError::Request {
                path: path.to_string(),
                message: error.to_string(),
            })?;
        self.send(request, path).await
    }

    async fn send_json(
        &self,
        method: &str,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<Response<AsyncBody>, ApiError> {
        let bytes = serde_json::to_vec(body).map_err(|error| ApiError::Encode {
            path: path.to_string(),
            message: error.to_string(),
        })?;
        let request = Request::builder()
            .method(method)
            .uri(self.url(path))
            .header("content-type", "application/json")
            .body(AsyncBody::from(bytes))
            .map_err(|error| ApiError::Request {
                path: path.to_string(),
                message: error.to_string(),
            })?;
        self.send(request, path).await
    }

    async fn send(
        &self,
        request: Request<AsyncBody>,
        path: &str,
    ) -> Result<Response<AsyncBody>, ApiError> {
        self.http
            .send(request)
            .await
            .map_err(|error| ApiError::Request {
                path: path.to_string(),
                message: format!("{error:#}"),
            })
    }
}

/// `askUser` 问卷的端点：路由由 `astrcode-ask-user` 扩展自己注册。
fn ask_user_url(session_id: &str, call_id: &str, action: &str) -> String {
    format!("/api/extensions/astrcode-ask-user/sessions/{session_id}/questions/{call_id}/{action}")
}

/// 跨会话的待回答问卷快照；同属 `astrcode-ask-user` 自己的路由。
const PENDING_ASK_USER_PATH: &str = "/api/extensions/astrcode-ask-user/questions";

/// 看板扩展的路由前缀：路由由 `astrcode-kanban` 自己注册。
fn kanban_url(suffix: &str) -> String {
    format!("/api/extensions/{}{suffix}", crate::kanban::EXTENSION_ID)
}

/// 代码浏览接口的 URL：根目录与相对路径都作为查询参数跨边界，字符集不受我们控制
/// （路径里可以有空格、`#`、`&`），因此两个都过一遍百分号编码。
fn files_url(endpoint: &str, root: &str, path: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("root", root)
        .append_pair("path", path)
        .finish();
    format!("/api/files{endpoint}?{query}")
}

/// 只要根目录的代码接口 URL：工作区清单与单个路径无关。
fn files_root_url(endpoint: &str, root: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("root", root)
        .finish();
    format!("/api/files{endpoint}?{query}")
}

/// 只关心成败的请求：非 2xx 带上响应体转成 [`ApiError::Status`]。
async fn require_success(path: &str, response: Response<AsyncBody>) -> Result<(), ApiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = read_text(path, response).await.unwrap_or_default();
    Err(ApiError::Status {
        status: status.as_u16(),
        body,
    })
}

async fn decode_json<T: serde::de::DeserializeOwned>(
    path: &str,
    response: Response<AsyncBody>,
) -> Result<T, ApiError> {
    let status = response.status();
    let body = read_text(path, response).await?;
    if !status.is_success() {
        return Err(ApiError::Status {
            status: status.as_u16(),
            body,
        });
    }
    serde_json::from_str(&body).map_err(|error| ApiError::Decode {
        path: path.to_string(),
        message: error.to_string(),
    })
}

async fn read_text(path: &str, response: Response<AsyncBody>) -> Result<String, ApiError> {
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes)
        .await
        .map_err(|error| ApiError::Request {
            path: path.to_string(),
            message: error.to_string(),
        })?;
    String::from_utf8(bytes).map_err(|error| ApiError::Decode {
        path: path.to_string(),
        message: error.to_string(),
    })
}

/// 会话事件流。
pub struct ConversationStream {
    path: String,
    body: AsyncBody,
    decoder: SseDecoder,
    /// 已解出但尚未交出的原始帧。
    ///
    /// 一次网络读取可能带来多帧，`SseDecoder` 会把它们全部吐出，所以必须自己排队，
    /// 否则除第一帧外全被丢掉。
    pending: VecDeque<String>,
    finished: bool,
}

impl ConversationStream {
    /// 取下一条信封；流正常结束时返回 `Ok(None)`。
    pub async fn next(&mut self) -> Result<Option<ConversationStreamEnvelopeDto>, ApiError> {
        loop {
            if let Some(envelope) = self.try_next()? {
                return Ok(Some(envelope));
            }
            if self.finished {
                // 服务端关闭时可能留下没有收尾空行的半帧，按无效帧丢弃。
                self.decoder.discard_partial();
                return Ok(None);
            }
            let mut chunk = [0u8; 8192];
            match self.body.read(&mut chunk).await {
                Ok(0) => self.finished = true,
                Ok(n) => {
                    self.decoder.push(&chunk[..n]);
                    self.pending.extend(self.decoder.take_frames());
                },
                Err(error) => {
                    return Err(ApiError::Request {
                        path: self.path.clone(),
                        message: error.to_string(),
                    });
                },
            }
        }
    }

    /// 取一条**已经**解出的信封，不触发网络读；队列空时返回 `Ok(None)`。
    ///
    /// 调用方据此把一次唤醒里已就绪的帧攒成一批再渲染（ADR 0001 第 13 轮）。
    pub fn try_next(&mut self) -> Result<Option<ConversationStreamEnvelopeDto>, ApiError> {
        let Some(frame) = self.pending.pop_front() else {
            return Ok(None);
        };
        let envelope = serde_json::from_str(&frame).map_err(|error| ApiError::Decode {
            path: self.path.clone(),
            message: error.to_string(),
        })?;
        Ok(Some(envelope))
    }
}

/// SSE 分帧解码器。
///
/// 服务端的每帧是 `event: conversation\ndata: <json>\n\n`。分帧按字节做，因为网络
/// 分片可能切在 UTF-8 序列中间；帧边界是 ASCII 的 `\n\n`，所以切分本身安全。
#[derive(Default)]
pub struct SseDecoder {
    pending: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) {
        self.pending.extend_from_slice(chunk);
    }

    /// 取出已完整成帧的 `data:` 载荷。
    fn take_frames(&mut self) -> Vec<String> {
        let mut frames = Vec::new();
        while let Some(index) = find_frame_end(&self.pending) {
            let frame: Vec<u8> = self.pending.drain(..index + 2).collect();
            frames.extend(frame_data(&frame));
        }
        frames
    }

    fn discard_partial(&mut self) {
        self.pending.clear();
    }
}

fn find_frame_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\n\n")
}

fn frame_data(frame: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(frame).ok()?;
    let data = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    (!data.is_empty()).then_some(data)
}

#[cfg(test)]
mod tests {
    use super::SseDecoder;

    #[test]
    fn frames_split_across_chunks_are_reassembled() {
        let mut decoder = SseDecoder::default();
        decoder.push(b"event: conversation\ndata: {\"a\"");
        assert!(decoder.take_frames().is_empty());
        decoder.push(b":1}\n\n");
        assert_eq!(decoder.take_frames(), vec!["{\"a\":1}".to_string()]);
    }

    #[test]
    fn multi_byte_characters_survive_arbitrary_chunk_boundaries() {
        let frame = "event: conversation\ndata: {\"text\":\"看板\"}\n\n"
            .as_bytes()
            .to_vec();
        for split in 1..frame.len() {
            let mut decoder = SseDecoder::default();
            decoder.push(&frame[..split]);
            decoder.push(&frame[split..]);
            assert_eq!(
                decoder.take_frames(),
                vec!["{\"text\":\"看板\"}".to_string()],
                "split at {split} lost the frame"
            );
        }
    }

    #[test]
    fn multiple_frames_in_one_chunk_are_all_returned() {
        let mut decoder = SseDecoder::default();
        decoder.push(b"data: 1\n\ndata: 2\n\n");
        assert_eq!(
            decoder.take_frames(),
            vec!["1".to_string(), "2".to_string()]
        );
    }
}

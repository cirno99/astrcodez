//! 设置页的领域推导，对应 Web 前端的 `Settings/settingsSupport.ts`。
//!
//! 按迁移文档的验收基线，thinking 能力 → 表单值 → API 请求这层映射是领域不变式，
//! 必须一对一重建并测试；这里只做推导，全部不碰 gpui——设置页渲染在 `views::settings`。
//!
//! 刻意没跟的：`appearance` 分区与 `THEME_OPTIONS` 不重建——迁移文档第 11 轮已定
//! 「外观分区不照搬，桌面端主题语义不同」；主题切换由外壳自己处理，不走设置页。
//!
//! 错误直接返回中文字符串：前端这几个校验分支的错误文案就是给用户看的，
//! 逐字对齐（`thinkingFormToRequest` 的各 throw）。

use astrcode_protocol::{
    http::{
        ConfigViewResponseDto, ExtensionDependencyKindDto, ExtensionServiceBlockDto,
        ExtensionServiceDependencyDto, ExtensionStateDto, ModelDto, ProfileDto, ProviderSpecDto,
        ThinkingConfigDto, UpdateModelOptionsRequest,
    },
    wire::{
        ExtensionSourceDto, ProviderAuthSchemeDto, ProviderWireFormatDto, ThinkingCapabilityDto,
    },
};

/// 设置页的分区。前端有四个，`appearance` 不照搬（见模块注释）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Models,
    Providers,
    Permissions,
    Plugins,
}

impl SettingsSection {
    /// 分区标题，与前端 `SETTINGS_NAV_ITEMS` 的 `label` 一致。
    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::Models => "模型",
            SettingsSection::Providers => "Providers",
            SettingsSection::Permissions => "权限",
            SettingsSection::Plugins => "插件",
        }
    }

    /// 分区说明，与前端 `SETTINGS_NAV_ITEMS` 的 `hint` 一致。
    pub fn hint(self) -> &'static str {
        match self {
            SettingsSection::Models => "当前主模型与小模型",
            SettingsSection::Providers => "所有已配置和预设",
            SettingsSection::Permissions => "工具批准策略",
            SettingsSection::Plugins => "扩展状态与启停",
        }
    }
}

/// 思考（thinking）表单的三种模式：恢复默认、显式开启、显式关闭。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThinkingFormMode {
    #[default]
    Default,
    Enabled,
    Disabled,
}

/// thinking 表单值：模式 + effort 字符串 + 预算 Token 字符串。
///
/// effort 与预算保持字符串形态，与前端一致——输入框里是文本，有效性在提交时校验。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThinkingFormValue {
    pub mode: ThinkingFormMode,
    pub effort: String,
    pub budget_tokens: String,
}

/// thinking 表单的初始形态：恢复默认、无 effort、无预算。
pub fn default_thinking_form() -> ThinkingFormValue {
    ThinkingFormValue {
        mode: ThinkingFormMode::Default,
        effort: String::new(),
        budget_tokens: String::new(),
    }
}

/// effort 线缆值的中文标签；未知值原样展示（与前端 `EFFORT_LABELS` 同口径）。
pub fn effort_label(value: &str) -> &str {
    match value {
        "low" => "低",
        "medium" => "中",
        "high" => "高",
        "minimal" => "最小",
        "max" => "最大",
        _ => value,
    }
}

/// 从模型的 thinking 能力与当前配置推导表单初始值。
///
/// 模型未声明能力、或当前无配置时都回默认——与前端 `deriveThinkingFormValue` 逐支一致：
/// `canDisable == false` 的模型即便配置处于关闭态也回默认（它不支持关闭，关闭态不是合法输入）。
pub fn derive_thinking_form_value(
    capability: Option<&ThinkingCapabilityDto>,
    current: Option<&ThinkingConfigDto>,
) -> ThinkingFormValue {
    let (Some(capability), Some(current)) = (capability, current) else {
        return default_thinking_form();
    };
    if current.enabled {
        return ThinkingFormValue {
            mode: ThinkingFormMode::Enabled,
            effort: current.effort.clone().unwrap_or_default(),
            budget_tokens: current
                .budget_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_default(),
        };
    }
    if !capability.can_disable {
        return default_thinking_form();
    }
    ThinkingFormValue {
        mode: ThinkingFormMode::Disabled,
        effort: String::new(),
        budget_tokens: String::new(),
    }
}

/// 把表单值映射为 API 请求体；校验失败给中文错误文案。
///
/// `default` → `thinking: None` 恢复默认；`disabled` → `{ enabled: false }`；
/// `enabled` → `{ enabled: true }` 附带 effort / 预算。能力校验逐字对齐前端：
/// 无能力声明直接拒绝，allowedEffort 非空时必选、为空时必不填，预算只在能力
/// 声明了下限或上限时接受，且必须落在声明区间内。
pub fn thinking_form_to_request(
    profile_name: &str,
    model_id: &str,
    form: &ThinkingFormValue,
    capability: Option<&ThinkingCapabilityDto>,
) -> Result<UpdateModelOptionsRequest, String> {
    let mut request = UpdateModelOptionsRequest {
        profile_name: profile_name.to_owned(),
        model_id: model_id.to_owned(),
        thinking: None,
    };
    if form.mode == ThinkingFormMode::Default {
        return Ok(request);
    }
    let Some(capability) = capability else {
        return Err("此模型未声明 Thinking 能力".to_owned());
    };
    if form.mode == ThinkingFormMode::Disabled {
        if !capability.can_disable {
            return Err("此模型不支持关闭 Thinking".to_owned());
        }
        request.thinking = Some(ThinkingConfigDto {
            enabled: false,
            effort: None,
            budget_tokens: None,
        });
        return Ok(request);
    }
    if let Some(allowed_effort) = &capability.allowed_effort {
        if !allowed_effort.is_empty() && form.effort.is_empty() {
            return Err("请选择思考努力层级".to_owned());
        }
        if allowed_effort.is_empty() && !form.effort.is_empty() {
            return Err("此模型不支持设置思考努力层级".to_owned());
        }
        if !form.effort.is_empty() && !allowed_effort.contains(&form.effort) {
            return Err(format!("不支持的思考努力层级：{}", form.effort));
        }
    }
    let supports_budget = capability.budget_min.is_some() || capability.budget_max.is_some();
    let requires_budget = capability.budget_min.is_some();
    if requires_budget && form.budget_tokens.is_empty() {
        return Err("请输入思考预算 Token".to_owned());
    }
    if !supports_budget && !form.budget_tokens.is_empty() {
        return Err("此模型不支持设置思考预算 Token".to_owned());
    }
    let mut thinking = ThinkingConfigDto {
        enabled: true,
        effort: None,
        budget_tokens: None,
    };
    if !form.effort.is_empty() {
        thinking.effort = Some(form.effort.clone());
    }
    if !form.budget_tokens.is_empty() {
        let budget_tokens: u32 = form
            .budget_tokens
            .trim()
            .parse()
            .map_err(|_| "思考预算 Token 必须是正整数".to_owned())?;
        if budget_tokens == 0 {
            return Err("思考预算 Token 必须是正整数".to_owned());
        }
        if let Some(min) = capability.budget_min
            && budget_tokens < min
        {
            return Err(format!("思考预算 Token 不能小于 {min}"));
        }
        if let Some(max) = capability.budget_max
            && budget_tokens > max
        {
            return Err(format!("思考预算 Token 不能大于 {max}"));
        }
        thinking.budget_tokens = Some(budget_tokens);
    }
    request.thinking = Some(thinking);
    Ok(request)
}

/// 判断模型是否只有 toggle（无 effort / 预算控制）：能力声明存在、
/// 但既没给可选 effort 也没给预算区间。
///
/// 与前端同口径：`allowedEffort == null` 视为有 effort（未知值交给模型自己协商），
/// 空数组才是「不支持」。
pub fn is_toggle_only_thinking(capability: Option<&ThinkingCapabilityDto>) -> bool {
    let Some(capability) = capability else {
        return false;
    };
    let has_effort = capability
        .allowed_effort
        .as_ref()
        .is_none_or(|efforts| !efforts.is_empty());
    let has_budget = capability.budget_min.is_some() || capability.budget_max.is_some();
    !has_effort && !has_budget
}

/// 设置页里当前选中的模型组合。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelSelection {
    pub profile_name: String,
    pub model_id: String,
    pub small_profile_name: String,
    pub small_model_id: String,
    pub thinking_form: ThinkingFormValue,
}

/// 从配置视图推导当前选区，thinking 表单按主模型当前配置初始化。
pub fn model_selection_from_config(config: &ConfigViewResponseDto) -> ModelSelection {
    let model = model_of(config, &config.active_profile, &config.active_model);
    ModelSelection {
        profile_name: config.active_profile.clone(),
        model_id: config.active_model.clone(),
        small_profile_name: config.active_small_profile.clone().unwrap_or_default(),
        small_model_id: config.active_small_model.clone().unwrap_or_default(),
        thinking_form: derive_thinking_form_value(
            model
                .as_ref()
                .and_then(|model| model.thinking_capability.as_ref()),
            model.as_ref().and_then(|model| model.thinking.as_ref()),
        ),
    }
}

/// 换 profile 或模型后重新推导 thinking 表单（前端 `deriveModelThinkingForm`）。
pub fn derive_model_thinking_form(
    config: &ConfigViewResponseDto,
    profile_name: &str,
    model_id: &str,
) -> ThinkingFormValue {
    match model_of(config, profile_name, model_id) {
        Some(model) => {
            derive_thinking_form_value(model.thinking_capability.as_ref(), model.thinking.as_ref())
        },
        None => default_thinking_form(),
    }
}

fn model_of(
    config: &ConfigViewResponseDto,
    profile_name: &str,
    model_id: &str,
) -> Option<ModelDto> {
    let profile = profile_of(config, profile_name)?;
    profile
        .models
        .iter()
        .find(|model| model.id == model_id)
        .cloned()
}

fn profile_of<'a>(config: &'a ConfigViewResponseDto, profile_name: &str) -> Option<&'a ProfileDto> {
    config
        .profiles
        .iter()
        .find(|profile| profile.name == profile_name)
}

/// 在一个 profile 里挑模型：当前值还在列表里就用它，否则用第一个。
///
/// 与前端 `pickModel` 同判据；profile 不存在或没有模型时给空串。
pub fn pick_model(profile: Option<&ProfileDto>, current_model: &str) -> String {
    let Some(profile) = profile else {
        return String::new();
    };
    if profile.models.is_empty() {
        return String::new();
    }
    if profile.models.iter().any(|model| model.id == current_model) {
        return current_model.to_owned();
    }
    profile.models[0].id.clone()
}

/// Provider 线缆格式的中文标签（前端 `providerWireFormatLabel`）。
pub fn wire_format_label(format: ProviderWireFormatDto) -> &'static str {
    match format {
        ProviderWireFormatDto::OpenAiChatCompletions => "OpenAI Chat Completions",
        ProviderWireFormatDto::OpenAiResponses => "OpenAI Responses",
        ProviderWireFormatDto::AnthropicMessages => "Anthropic Messages",
    }
}

/// Provider 认证方式的中文标签（前端 `providerAuthSchemeLabel`）。
pub fn auth_scheme_label(scheme: ProviderAuthSchemeDto) -> &'static str {
    match scheme {
        ProviderAuthSchemeDto::None => "无需认证",
        ProviderAuthSchemeDto::Bearer => "Bearer Token",
        ProviderAuthSchemeDto::XApiKey => "x-api-key",
    }
}

/// 扩展此刻的状态，对应前端插件页 `statusLabel` 的四支。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionStatus {
    /// 被配置禁用。
    Disabled,
    /// 依赖的服务没有可用提供者，装载被挡下。
    Blocked,
    /// 启用着但没能装载。
    Unloaded,
    /// 已装载。
    Loaded,
}

impl ExtensionStatus {
    /// 状态文案（前端 `statusLabel`）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Disabled => "已禁用",
            Self::Blocked => "依赖阻塞",
            Self::Unloaded => "未加载",
            Self::Loaded => "已加载",
        }
    }
}

/// 推导扩展状态；分支顺序与前端 `statusLabel` 一致——「依赖阻塞」压过「未加载」。
pub fn extension_status(extension: &ExtensionStateDto) -> ExtensionStatus {
    if !extension.enabled {
        return ExtensionStatus::Disabled;
    }
    let blocked = extension
        .declaration
        .as_ref()
        .is_some_and(|declaration| !declaration.blocked_reasons.is_empty());
    if blocked {
        return ExtensionStatus::Blocked;
    }
    if extension.loaded {
        ExtensionStatus::Loaded
    } else {
        ExtensionStatus::Unloaded
    }
}

/// 扩展来源的中文标签（前端插件页 `sourceLabel`）。
pub fn extension_source_label(source: ExtensionSourceDto) -> &'static str {
    match source {
        ExtensionSourceDto::Builtin => "内置",
        ExtensionSourceDto::Disk => "磁盘",
        ExtensionSourceDto::Unknown => "未知",
    }
}

/// 依赖行：每个依赖写成「服务（必需/可选）」，逗号相连。
pub fn extension_dependencies_label(dependencies: &[ExtensionServiceDependencyDto]) -> String {
    dependencies
        .iter()
        .map(|dependency| {
            let kind = match dependency.kind {
                ExtensionDependencyKindDto::Required => "必需",
                ExtensionDependencyKindDto::Optional => "可选",
            };
            format!("{}（{kind}）", dependency.service)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// 阻塞原因的一句说明（前端插件页 `blockedReasons` 的四支模板）。
pub fn blocked_reason_label(reason: &ExtensionServiceBlockDto) -> String {
    match reason {
        ExtensionServiceBlockDto::MissingService { service } => format!("缺少服务：{service}"),
        ExtensionServiceBlockDto::ProviderConflict { service, providers } => {
            format!("服务 {service} 的提供者冲突：{}", providers.join(", "))
        },
        ExtensionServiceBlockDto::DependencyCycle { members } => {
            format!("循环依赖：{}", members.join(" → "))
        },
        ExtensionServiceBlockDto::DependencyBlocked { provider } => {
            format!("上游插件受阻：{provider}")
        },
    }
}

/// 归一化 base URL：去空白、去尾部斜杠、转小写（前端 `normalizeBaseUrl`）。
///
/// 只用于 profile 与 provider 预设 endpoint 的对号，不校验合法性。
fn normalize_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_lowercase()
}

/// profile 的 base URL 是否落在 provider 预设声明的 endpoint 里（前端
/// `profileMatchesProviderEndpoint`）。
fn profile_matches_provider_endpoint(profile: &ProfileDto, provider: &ProviderSpecDto) -> bool {
    let profile_url = normalize_base_url(&profile.base_url);
    !profile_url.is_empty()
        && provider.endpoints.iter().any(|endpoint| {
            endpoint
                .base_url
                .as_deref()
                .is_some_and(|url| normalize_base_url(url) == profile_url)
        })
}

/// 找到一个 provider 预设已经配置成的 profile（前端 `findProviderProfile`）。
///
/// 三段匹配：名字直等 → providerKind + wireFormat → wireFormat + authScheme + endpoint 对上。
pub fn find_provider_profile<'a>(
    profiles: &'a [ProfileDto],
    provider: &ProviderSpecDto,
) -> Option<&'a ProfileDto> {
    profiles
        .iter()
        .find(|profile| profile.name == provider.id)
        .or_else(|| {
            profiles.iter().find(|profile| {
                profile.provider_kind == provider.provider_kind
                    && profile.wire_format == provider.wire_format
            })
        })
        .or_else(|| {
            profiles.iter().find(|profile| {
                profile.wire_format == provider.wire_format
                    && profile.auth_scheme == provider.auth_scheme
                    && profile_matches_provider_endpoint(profile, provider)
            })
        })
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::{
        http::{ExtensionDeclarationDto, ProviderEndpointPresetDto, ProviderSpecCapabilitiesDto},
        wire::ApprovalModeDto,
    };

    use super::*;

    fn capability(
        allowed_effort: Option<Vec<&str>>,
        budget_min: Option<u32>,
        budget_max: Option<u32>,
        can_disable: bool,
    ) -> ThinkingCapabilityDto {
        ThinkingCapabilityDto {
            allowed_effort: allowed_effort
                .map(|efforts| efforts.into_iter().map(str::to_owned).collect()),
            budget_min,
            budget_max,
            can_disable,
        }
    }

    fn config(profiles: Vec<ProfileDto>, active: (&str, &str)) -> ConfigViewResponseDto {
        ConfigViewResponseDto {
            config_path: String::new(),
            active_profile: active.0.to_owned(),
            active_model: active.1.to_owned(),
            active_small_profile: None,
            active_small_model: None,
            approval_mode: ApprovalModeDto::Manual,
            profiles,
            warning: None,
        }
    }

    fn profile(name: &str, models: Vec<ModelDto>) -> ProfileDto {
        ProfileDto {
            name: name.to_owned(),
            provider_kind: "openai".to_owned(),
            wire_format: ProviderWireFormatDto::OpenAiChatCompletions,
            auth_scheme: ProviderAuthSchemeDto::Bearer,
            base_url: String::new(),
            has_api_key: true,
            models,
        }
    }

    fn model(id: &str) -> ModelDto {
        ModelDto {
            id: id.to_owned(),
            model_options: None,
            thinking: None,
            thinking_capability: None,
        }
    }

    #[test]
    fn derive_defaults_without_capability_or_config() {
        let capability = capability(Some(vec!["low"]), None, None, true);
        // 无能力声明、无配置各回默认：模型不支持 thinking 时表单不参与渲染。
        let current = ThinkingConfigDto {
            enabled: false,
            effort: None,
            budget_tokens: None,
        };
        assert_eq!(
            derive_thinking_form_value(None, Some(&current)),
            default_thinking_form()
        );
        assert_eq!(
            derive_thinking_form_value(Some(&capability), None),
            default_thinking_form()
        );
    }

    #[test]
    fn derive_keeps_enabled_state_fields() {
        let capability = capability(Some(vec!["low", "high"]), Some(1024), Some(8192), true);
        let current = ThinkingConfigDto {
            enabled: true,
            effort: Some("high".to_owned()),
            budget_tokens: Some(4096),
        };
        let form = derive_thinking_form_value(Some(&capability), Some(&current));
        assert_eq!(form.mode, ThinkingFormMode::Enabled);
        assert_eq!(form.effort, "high");
        assert_eq!(form.budget_tokens, "4096");
    }

    #[test]
    fn derive_maps_disabled_and_refuses_it_when_uncancellable() {
        let cancellable = capability(None, None, None, true);
        let current = ThinkingConfigDto {
            enabled: false,
            effort: None,
            budget_tokens: None,
        };
        assert_eq!(
            derive_thinking_form_value(Some(&cancellable), Some(&current)).mode,
            ThinkingFormMode::Disabled
        );
        // 不支持关闭的模型，配置关闭态不是合法输入，回默认。
        let locked = capability(None, None, None, false);
        assert_eq!(
            derive_thinking_form_value(Some(&locked), Some(&current)),
            default_thinking_form()
        );
    }

    #[test]
    fn request_default_restores_and_disabled_requires_permission() {
        let form = default_thinking_form();
        let request = thinking_form_to_request("p", "m", &form, None).unwrap();
        assert!(request.thinking.is_none());
        assert_eq!(request.profile_name, "p");
        assert_eq!(request.model_id, "m");

        // 无能力声明时开/关都被拒绝。
        let mut disabled = default_thinking_form();
        disabled.mode = ThinkingFormMode::Disabled;
        assert_eq!(
            thinking_form_to_request("p", "m", &disabled, None).unwrap_err(),
            "此模型未声明 Thinking 能力"
        );

        // 不支持关闭的模型不能关。
        let locked = capability(None, None, None, false);
        assert_eq!(
            thinking_form_to_request("p", "m", &disabled, Some(&locked)).unwrap_err(),
            "此模型不支持关闭 Thinking"
        );
        let open = capability(None, None, None, true);
        let request = thinking_form_to_request("p", "m", &disabled, Some(&open)).unwrap();
        assert!(!request.thinking.as_ref().unwrap().enabled);
    }

    #[test]
    fn request_validates_effort_against_allowed_list() {
        let mut form = default_thinking_form();
        form.mode = ThinkingFormMode::Enabled;
        let effort_cap = capability(Some(vec!["low", "high"]), None, None, true);

        form.effort = String::new();
        assert_eq!(
            thinking_form_to_request("p", "m", &form, Some(&effort_cap)).unwrap_err(),
            "请选择思考努力层级"
        );

        form.effort = "max".to_owned();
        assert_eq!(
            thinking_form_to_request("p", "m", &form, Some(&effort_cap)).unwrap_err(),
            "不支持的思考努力层级：max"
        );

        form.effort = "high".to_owned();
        let request = thinking_form_to_request("p", "m", &form, Some(&effort_cap)).unwrap();
        assert_eq!(
            request.thinking.as_ref().unwrap().effort.as_deref(),
            Some("high")
        );

        // 声明了空数组却给了 effort：这个模型不支持设置层级。
        let no_effort = capability(Some(vec![]), None, None, true);
        assert_eq!(
            thinking_form_to_request("p", "m", &form, Some(&no_effort)).unwrap_err(),
            "此模型不支持设置思考努力层级"
        );
    }

    #[test]
    fn request_validates_budget_bounds() {
        let mut form = default_thinking_form();
        form.mode = ThinkingFormMode::Enabled;
        let budget_cap = capability(None, Some(1024), Some(8192), true);

        form.budget_tokens = String::new();
        assert_eq!(
            thinking_form_to_request("p", "m", &form, Some(&budget_cap)).unwrap_err(),
            "请输入思考预算 Token"
        );

        form.budget_tokens = "512".to_owned();
        assert_eq!(
            thinking_form_to_request("p", "m", &form, Some(&budget_cap)).unwrap_err(),
            "思考预算 Token 不能小于 1024"
        );

        form.budget_tokens = "99999".to_owned();
        assert_eq!(
            thinking_form_to_request("p", "m", &form, Some(&budget_cap)).unwrap_err(),
            "思考预算 Token 不能大于 8192"
        );

        form.budget_tokens = "4096".to_owned();
        let request = thinking_form_to_request("p", "m", &form, Some(&budget_cap)).unwrap();
        assert_eq!(request.thinking.as_ref().unwrap().budget_tokens, Some(4096));

        // 非正整数被拒绝；能力没声明预算时给了预算也被拒绝。
        form.budget_tokens = "0".to_owned();
        assert_eq!(
            thinking_form_to_request("p", "m", &form, Some(&budget_cap)).unwrap_err(),
            "思考预算 Token 必须是正整数"
        );
        form.budget_tokens = "4096".to_owned();
        assert_eq!(
            thinking_form_to_request("p", "m", &form, Some(&capability(None, None, None, true)))
                .unwrap_err(),
            "此模型不支持设置思考预算 Token"
        );
    }

    #[test]
    fn toggle_only_and_effort_labels() {
        assert!(!is_toggle_only_thinking(None));
        assert!(is_toggle_only_thinking(Some(&capability(
            Some(vec![]),
            None,
            None,
            true
        ))));
        assert!(!is_toggle_only_thinking(Some(&capability(
            Some(vec!["low"]),
            None,
            None,
            true
        ))));
        assert!(!is_toggle_only_thinking(Some(&capability(
            None,
            Some(1),
            None,
            true
        ))));

        assert_eq!(effort_label("low"), "低");
        assert_eq!(effort_label("max"), "最大");
        assert_eq!(effort_label("ultra"), "ultra");
    }

    #[test]
    fn selection_follows_active_config_and_rederives_on_change() {
        let mut main_model = model("m-1");
        main_model.thinking_capability = Some(capability(Some(vec!["low"]), None, None, true));
        let config = config(
            vec![profile("p", vec![main_model, model("m-2")])],
            ("p", "m-1"),
        );
        let selection = model_selection_from_config(&config);
        assert_eq!(selection.profile_name, "p");
        assert_eq!(selection.model_id, "m-1");
        // 配置里还没写 thinking：表单回默认。
        assert_eq!(selection.thinking_form, default_thinking_form());

        // 换模型后按新模型的配置重推。
        assert_eq!(
            derive_model_thinking_form(&config, "p", "m-2"),
            default_thinking_form()
        );
        assert_eq!(
            derive_model_thinking_form(&config, "unknown", "m-2"),
            default_thinking_form()
        );
    }

    #[test]
    fn pick_model_falls_back_to_the_first_model() {
        let cfg = config(
            vec![profile("p", vec![model("m-1"), model("m-2")])],
            ("p", "m-1"),
        );
        let selected = profile_of(&cfg, "p");
        assert_eq!(pick_model(selected, "m-2"), "m-2");
        assert_eq!(pick_model(selected, "gone"), "m-1");
        assert_eq!(pick_model(selected, ""), "m-1");
        assert_eq!(pick_model(None, "m-1"), "");
        let empty_config = config(vec![profile("q", vec![])], ("q", ""));
        assert_eq!(pick_model(profile_of(&empty_config, "q"), ""), "");
    }

    fn provider_spec(
        id: &str,
        kind: &str,
        wire: ProviderWireFormatDto,
        auth: ProviderAuthSchemeDto,
        base_urls: Vec<&str>,
    ) -> ProviderSpecDto {
        ProviderSpecDto {
            id: id.to_owned(),
            display_name: id.to_owned(),
            provider_kind: kind.to_owned(),
            wire_format: wire,
            auth_scheme: auth,
            default_model: String::new(),
            api_key_env_vars: vec![],
            endpoints: base_urls
                .into_iter()
                .map(|url| ProviderEndpointPresetDto {
                    id: url.to_owned(),
                    label: url.to_owned(),
                    base_url: Some(url.to_owned()),
                    is_default: false,
                })
                .collect(),
            capabilities: ProviderSpecCapabilitiesDto {
                prompt_cache_key: false,
                stream_usage: false,
                reasoning_effort: false,
                strict_tool_use: false,
            },
        }
    }

    #[test]
    fn find_provider_profile_matches_in_three_stages() {
        let profiles = vec![
            profile("other", vec![model("m")]),
            profile("by-endpoint", vec![model("m")]),
        ];

        // 名字直等优先。
        let mut by_name = profile("presets-id", vec![model("m")]);
        by_name.wire_format = ProviderWireFormatDto::OpenAiResponses;
        let profiles = [profiles, vec![by_name]].concat();
        let spec = provider_spec(
            "presets-id",
            "openai",
            ProviderWireFormatDto::OpenAiChatCompletions,
            ProviderAuthSchemeDto::Bearer,
            vec![],
        );
        assert_eq!(
            find_provider_profile(&profiles, &spec).unwrap().name,
            "presets-id"
        );

        // 名字对不上时按 providerKind + wireFormat 找。
        let spec = provider_spec(
            "some-id",
            "openai",
            ProviderWireFormatDto::OpenAiChatCompletions,
            ProviderAuthSchemeDto::Bearer,
            vec![],
        );
        assert_eq!(
            find_provider_profile(&profiles, &spec).unwrap().name,
            "other"
        );

        // 都对不上时按 wireFormat + authScheme + endpoint 对号，URL 归一化后比对。
        let mut endpoint_profile = profile("by-endpoint", vec![model("m")]);
        endpoint_profile.wire_format = ProviderWireFormatDto::AnthropicMessages;
        endpoint_profile.auth_scheme = ProviderAuthSchemeDto::XApiKey;
        endpoint_profile.base_url = "https://Api.Example.com/v1/".to_owned();
        let profiles = [profiles, vec![endpoint_profile]].concat();
        let spec = provider_spec(
            "gone-id",
            "openai",
            ProviderWireFormatDto::AnthropicMessages,
            ProviderAuthSchemeDto::XApiKey,
            vec!["https://api.example.com/V1"],
        );
        assert_eq!(
            find_provider_profile(&profiles, &spec).unwrap().name,
            "by-endpoint"
        );

        // 哪一段都够不上：没有匹配。
        let spec = provider_spec(
            "gone-id",
            "coze",
            ProviderWireFormatDto::AnthropicMessages,
            ProviderAuthSchemeDto::None,
            vec![],
        );
        assert!(find_provider_profile(&profiles, &spec).is_none());
    }

    fn declaration(blocked_reasons: Vec<ExtensionServiceBlockDto>) -> ExtensionDeclarationDto {
        ExtensionDeclarationDto {
            services: Vec::new(),
            dependencies: Vec::new(),
            service_permissions: Vec::new(),
            blocked_reasons,
            id: "astrcode-kanban".to_owned(),
            capabilities: Vec::new(),
            required_transport_features: Vec::new(),
            tools: Vec::new(),
            dynamic_tools: false,
            commands: Vec::new(),
            dynamic_commands: false,
            keybindings: Vec::new(),
            status_items: Vec::new(),
            custom_events: Vec::new(),
            custom_event_subscriptions: Vec::new(),
            http_routes: Vec::new(),
        }
    }

    fn extension(
        enabled: bool,
        loaded: bool,
        blocked_reasons: Vec<ExtensionServiceBlockDto>,
    ) -> ExtensionStateDto {
        ExtensionStateDto {
            extension_id: "astrcode-kanban".to_owned(),
            enabled,
            loaded,
            source: ExtensionSourceDto::Builtin,
            declaration: Some(declaration(blocked_reasons)),
            diagnostics: None,
        }
    }

    #[test]
    fn extension_status_follows_the_ported_branch_order() {
        // 禁用优先于其余三支。
        assert_eq!(
            extension_status(&extension(false, true, Vec::new())).label(),
            "已禁用"
        );
        // 依赖受阻压过「未加载」：这条扩展连装载都没轮到。
        assert_eq!(
            extension_status(&extension(
                true,
                false,
                vec![ExtensionServiceBlockDto::MissingService {
                    service: "astrcode-kanban-store".to_owned(),
                }],
            )),
            ExtensionStatus::Blocked
        );
        assert_eq!(
            extension_status(&extension(true, false, Vec::new())).label(),
            "未加载"
        );
        assert_eq!(
            extension_status(&extension(true, true, Vec::new())).label(),
            "已加载"
        );

        // 没带声明的扩展不算受阻，与前端 `declaration?.blockedReasons?.length` 同判据。
        let mut undeclared = extension(true, true, Vec::new());
        undeclared.declaration = None;
        assert_eq!(extension_status(&undeclared), ExtensionStatus::Loaded);
    }

    #[test]
    fn extension_labels_match_the_ported_copy() {
        assert_eq!(extension_source_label(ExtensionSourceDto::Builtin), "内置");
        assert_eq!(extension_source_label(ExtensionSourceDto::Disk), "磁盘");
        assert_eq!(extension_source_label(ExtensionSourceDto::Unknown), "未知");

        let dependencies = vec![
            ExtensionServiceDependencyDto {
                service: "astrcode-kanban-store".to_owned(),
                kind: ExtensionDependencyKindDto::Required,
            },
            ExtensionServiceDependencyDto {
                service: "astrcode-git".to_owned(),
                kind: ExtensionDependencyKindDto::Optional,
            },
        ];
        assert_eq!(
            extension_dependencies_label(&dependencies),
            "astrcode-kanban-store（必需）, astrcode-git（可选）"
        );

        assert_eq!(
            blocked_reason_label(&ExtensionServiceBlockDto::MissingService {
                service: "astrcode-store".to_owned(),
            }),
            "缺少服务：astrcode-store"
        );
        assert_eq!(
            blocked_reason_label(&ExtensionServiceBlockDto::ProviderConflict {
                service: "astrcode-store".to_owned(),
                providers: vec!["a".to_owned(), "b".to_owned()],
            }),
            "服务 astrcode-store 的提供者冲突：a, b"
        );
        assert_eq!(
            blocked_reason_label(&ExtensionServiceBlockDto::DependencyCycle {
                members: vec!["a".to_owned(), "b".to_owned()],
            }),
            "循环依赖：a → b"
        );
        assert_eq!(
            blocked_reason_label(&ExtensionServiceBlockDto::DependencyBlocked {
                provider: "astrcode-a".to_owned(),
            }),
            "上游插件受阻：astrcode-a"
        );
    }
}

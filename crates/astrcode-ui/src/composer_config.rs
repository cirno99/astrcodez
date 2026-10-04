//! 输入区工具条上由宿主配置驱动的控件：工具权限模式与模型选择。
//!
//! 推导与请求组装都在这里，不碰窗口：按钮上写什么、点一下换到哪一档、面板按什么分组与
//! 过滤、改动时其余字段怎么保持——都能脱窗口测试。渲染留在 `views::chat`。

use astrcode_protocol::{
    http::{
        AvailableModelDto, ConfigViewResponseDto, CurrentModelResponseDto,
        UpdateActiveSelectionRequest,
    },
    wire::{ApprovalModeDto, ProviderWireFormatDto},
};

/// 权限模式按钮上的字。
pub(crate) fn approval_label(mode: ApprovalModeDto) -> &'static str {
    match mode {
        ApprovalModeDto::Yolo => "完全访问",
        ApprovalModeDto::Manual => "请求批准",
    }
}

/// 权限模式按钮的悬停说明：说的是「点了会变成什么」，不是「现在是什么」。
pub(crate) fn approval_hint(mode: ApprovalModeDto) -> &'static str {
    match mode {
        ApprovalModeDto::Yolo => "当前为 YOLO / 完全访问，点击切换为手动确认",
        ApprovalModeDto::Manual => "当前为手动确认，点击切换为 YOLO / 完全访问",
    }
}

/// 点一下换到的那一档。
pub(crate) fn toggled_approval(mode: ApprovalModeDto) -> ApprovalModeDto {
    match mode {
        ApprovalModeDto::Yolo => ApprovalModeDto::Manual,
        ApprovalModeDto::Manual => ApprovalModeDto::Yolo,
    }
}

/// 组一个选区请求：只有调用方指定的那几项会变，其余照当前配置原样带上。
///
/// 服务端一次收下整套选区（`routes/config.rs::update_active_selection`），缺的字段会被
/// 当成「不动」，所以换权限模式那一路必须把当前模型带上。
pub(crate) fn selection_request(
    config: &ConfigViewResponseDto,
    profile_name: &str,
    model_id: &str,
    approval_mode: ApprovalModeDto,
) -> UpdateActiveSelectionRequest {
    UpdateActiveSelectionRequest {
        active_profile: profile_name.to_owned(),
        active_model: model_id.to_owned(),
        active_small_profile: config.active_small_profile.clone(),
        active_small_model: config.active_small_model.clone(),
        approval_mode,
    }
}

/// 线缆格式的显示名，与前端 `providerWireFormatLabel` 同口径。
pub(crate) fn wire_format_label(format: ProviderWireFormatDto) -> &'static str {
    match format {
        ProviderWireFormatDto::OpenAiChatCompletions => "OpenAI Chat",
        ProviderWireFormatDto::OpenAiResponses => "OpenAI Responses",
        ProviderWireFormatDto::AnthropicMessages => "Anthropic Messages",
    }
}

/// 模型按钮上写什么：取到当前模型就写它的 id，还没取到时按加载状态给一句话。
pub(crate) fn current_model_label(
    current: Option<&CurrentModelResponseDto>,
    loading: bool,
) -> &str {
    match current {
        Some(current) => current.model_id.as_str(),
        None if loading => "加载中…",
        None => "未选择",
    }
}

/// 面板里按 profile 分出的一段。
pub(crate) struct ModelGroup<'a> {
    pub profile_name: &'a str,
    pub wire_format: ProviderWireFormatDto,
    pub models: Vec<&'a AvailableModelDto>,
}

/// 按 profile 分组：段内保持原顺序，段与段的先后按首次出现的顺序。
///
/// 过滤口径照搬前端 `ModelSelector`——profile 名或模型 id 命中即留，空查询不过滤。
pub(crate) fn model_groups<'a>(
    models: &'a [AvailableModelDto],
    query: &str,
) -> Vec<ModelGroup<'a>> {
    let query = query.to_lowercase();
    let mut groups: Vec<ModelGroup<'a>> = Vec::new();
    for model in models {
        let matched = query.is_empty()
            || model.model_id.to_lowercase().contains(&query)
            || model.profile_name.to_lowercase().contains(&query);
        if !matched {
            continue;
        }
        match groups
            .iter_mut()
            .find(|group| group.profile_name == model.profile_name)
        {
            Some(group) => group.models.push(model),
            None => groups.push(ModelGroup {
                profile_name: &model.profile_name,
                wire_format: model.wire_format,
                models: vec![model],
            }),
        }
    }
    groups
}

/// 这一项是不是当前选中的那一个。profile 与模型 id 要同时对得上。
pub(crate) fn is_current_model(
    current: Option<&CurrentModelResponseDto>,
    model: &AvailableModelDto,
) -> bool {
    current.is_some_and(|current| {
        current.profile_name == model.profile_name && current.model_id == model.model_id
    })
}

/// 面板里一条都没画时那句提示。
pub(crate) fn empty_model_note(has_models: bool) -> &'static str {
    if has_models {
        "无结果"
    } else {
        "未配置模型"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> ConfigViewResponseDto {
        ConfigViewResponseDto {
            config_path: "/tmp/config.toml".to_owned(),
            active_profile: "main".to_owned(),
            active_model: "gpt-5".to_owned(),
            active_small_profile: Some("fast".to_owned()),
            active_small_model: Some("gpt-5-mini".to_owned()),
            approval_mode: ApprovalModeDto::Manual,
            profiles: Vec::new(),
            warning: None,
        }
    }

    #[test]
    fn approval_mode_toggles_and_reads_back() {
        assert_eq!(
            toggled_approval(ApprovalModeDto::Manual),
            ApprovalModeDto::Yolo
        );
        assert_eq!(
            toggled_approval(toggled_approval(ApprovalModeDto::Manual)),
            ApprovalModeDto::Manual
        );
        assert_eq!(approval_label(ApprovalModeDto::Yolo), "完全访问");
        assert_eq!(approval_label(ApprovalModeDto::Manual), "请求批准");
    }

    #[test]
    fn selection_request_keeps_the_model_and_small_selection() {
        let config = config();
        let request = selection_request(
            &config,
            &config.active_profile,
            &config.active_model,
            toggled_approval(config.approval_mode),
        );
        assert_eq!(request.active_profile, "main");
        assert_eq!(request.active_model, "gpt-5");
        assert_eq!(request.approval_mode, ApprovalModeDto::Yolo);
        assert_eq!(request.active_small_profile.as_deref(), Some("fast"));
        assert_eq!(request.active_small_model.as_deref(), Some("gpt-5-mini"));
    }

    fn model(profile: &str, id: &str) -> AvailableModelDto {
        AvailableModelDto {
            profile_name: profile.to_owned(),
            model_id: id.to_owned(),
            provider_kind: "openai".to_owned(),
            wire_format: ProviderWireFormatDto::OpenAiChatCompletions,
        }
    }

    fn current(profile: &str, id: &str) -> CurrentModelResponseDto {
        CurrentModelResponseDto {
            profile_name: profile.to_owned(),
            model_id: id.to_owned(),
            provider_kind: "openai".to_owned(),
            wire_format: ProviderWireFormatDto::OpenAiChatCompletions,
        }
    }

    #[test]
    fn model_groups_keep_first_seen_order_and_filter_by_profile_or_id() {
        let models = vec![
            model("main", "gpt-5"),
            model("fast", "mini"),
            model("main", "gpt-5-codex"),
        ];

        let groups = model_groups(&models, "");
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].profile_name, "main");
        assert_eq!(groups[0].models.len(), 2);
        assert_eq!(groups[1].profile_name, "fast");
        assert_eq!(groups[1].models.len(), 1);

        // 按 profile 名过滤，大小写不敏感。
        let groups = model_groups(&models, "FAST");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].profile_name, "fast");

        // 按模型 id 过滤只留下命中的那一条，段名跟着它走。
        let groups = model_groups(&models, "codex");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].profile_name, "main");
        assert_eq!(groups[0].models.len(), 1);
        assert_eq!(groups[0].models[0].model_id, "gpt-5-codex");

        assert!(model_groups(&models, "nope").is_empty());
    }

    #[test]
    fn current_model_matches_only_the_same_profile_and_id() {
        let models = [model("main", "gpt-5"), model("fast", "gpt-5")];
        let current = current("fast", "gpt-5");

        assert!(is_current_model(Some(&current), &models[1]));
        // 同名模型在另一个 profile 下不算选中。
        assert!(!is_current_model(Some(&current), &models[0]));
        assert!(!is_current_model(None, &models[0]));

        assert_eq!(current_model_label(Some(&current), false), "gpt-5");
        assert_eq!(current_model_label(None, true), "加载中…");
        assert_eq!(current_model_label(None, false), "未选择");
    }

    #[test]
    fn model_panel_labels_follow_the_wire_format_and_empty_state() {
        assert_eq!(
            wire_format_label(ProviderWireFormatDto::OpenAiChatCompletions),
            "OpenAI Chat"
        );
        assert_eq!(
            wire_format_label(ProviderWireFormatDto::AnthropicMessages),
            "Anthropic Messages"
        );
        assert_eq!(empty_model_note(true), "无结果");
        assert_eq!(empty_model_note(false), "未配置模型");
    }
}

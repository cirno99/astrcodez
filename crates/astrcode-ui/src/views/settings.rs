//! 设置页：模型 / Providers / 权限三个分区。
//!
//! 对应前端的 `Settings/SettingsPage.tsx` 与其分区组件。领域推导（选区、thinking 表单 →
//! 请求、provider 对号）全在 [`crate::settings`]，这里只把它摆上屏幕并跑请求。
//!
//! 写操作一律走「请求 → 重取配置视图」：界面上的选区因此永远等于服务端此刻的那一份，
//! 不存在本地与服务端两套状态。跑完会影响输入区工具条的写操作再发一条
//! [`SettingsEvent::ModelChanged`]，让外壳去刷新会话面板的模型与权限按钮
//! （前端对应 `bumpModelRefreshKey`）。
//!
//! 刻意没跟的：`appearance` 分区（见 [`crate::settings`] 的模块注释）。
//!
//! 与前端的一处分歧：前端的插件页是独立视图（`Plugins/PluginsPage.tsx`，从侧边栏与设置页页头
//! 的「插件」按钮进入），这里收成设置页的第四个分区。两个宿主的主区域都只认
//! [`crate::views::MainView`] 那三页，插件因此没有自己的容器可挂，编进设置页就不再为它多开
//! 一处入口；内容的判据与文案仍逐条照搬那个页面。

use astrcode_protocol::{
    http::{
        ApplyProviderPresetRequest, ConfigViewResponseDto, ExtensionStateDto, ModelDto, ProfileDto,
        ProviderSpecDto, RemoveProviderPresetRequest, UpdateActiveSelectionRequest,
    },
    wire::{ApprovalModeDto, ThinkingCapabilityDto},
};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    component::{
        ActiveTheme as _, Disableable as _, Size,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        h_flex,
        input::{Input, InputEvent, InputState},
        searchable_list::SearchableListItem,
        select::{Select, SelectEvent, SelectState},
        v_flex,
    },
    div, px,
};

use crate::{
    api::Api,
    icons::IconName,
    settings::{self, ModelSelection, SettingsSection, ThinkingFormMode, ThinkingFormValue},
    views::icon_button,
};

/// 内容列的宽度上限，与前端 `max-w-[1040px]` 同值。
const CONTENT_MAX_WIDTH: f32 = 1040.0;
/// 左侧分区导航的宽度，与前端 `lg:grid-cols-[176px_...]` 同值。
const NAV_WIDTH: f32 = 176.0;
/// 选择器的宽度：同一页里所有下拉同宽，右侧才对齐。
const SELECT_WIDTH: f32 = 280.0;
/// 配置弹窗的宽度，与前端 `w-[520px]` 同值。
const CONFIG_DIALOG_WIDTH: f32 = 520.0;
/// 移除弹窗的宽度，与前端 `w-[460px]` 同值。
const REMOVE_DIALOG_WIDTH: f32 = 460.0;
/// 遮罩层的内边距，与前端 `p-5` 同值；窗口比弹窗窄时靠它留边。
const OVERLAY_PADDING: f32 = 20.0;

/// 设置页对外的事件。
#[derive(Debug, Clone)]
pub enum SettingsEvent {
    /// 用户要求展开侧边栏（收起时页头上的那枚按钮）。
    ToggleSidebar,
    /// 主模型 / 权限 / provider 配置被改过：会话面板的工具条按钮过期了。
    ModelChanged,
    /// 扩展的启停被改过：外壳要重取扩展清单，看板入口跟着出现或消失。
    ExtensionsChanged,
}

/// 正在跑的一次写操作。
///
/// 同一时刻只允许一件（与前端 `operation` 一样）：按钮的禁用与文案都看它，因此它得记得
/// 够细——「切换中」只该出现在被点的那一行上。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Operation {
    Save,
    Reload,
    Test,
    ActivateProfile { profile_name: String },
    ApplyProvider { provider_id: String },
    RemoveProfile { profile_name: String },
    ReloadExtensions,
    SetExtension,
}

/// 操作结果条，与前端 `SettingsFeedbackView` 同形。
enum Feedback {
    /// 一句结论：保存、重载、切换、应用、移除。
    Success(String),
    Error(String),
    /// 连接测试：成功与否，加一句服务端的话。
    Test {
        success: bool,
        message: String,
    },
}

/// 下拉里的一个选项。
///
/// 显示名与线缆值分开：取值若靠反解人类可读的标签，改一行文案就会静默改掉行为。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    label: SharedString,
    value: String,
}

impl SearchableListItem for Choice {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &String {
        &self.value
    }
}

/// 六个下拉各自该有哪些条目；与上次推过去的不同才重新推一遍。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SelectItems {
    profile: Vec<Choice>,
    model: Vec<Choice>,
    small_profile: Vec<Choice>,
    small_model: Vec<Choice>,
    thinking_mode: Vec<Choice>,
    effort: Vec<Choice>,
}

/// 六个下拉此刻该显示的值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SelectionSnapshot {
    profile: String,
    model: String,
    small_profile: String,
    small_model: String,
    thinking_mode: ThinkingFormMode,
    effort: String,
}

/// 设置页里的六个下拉。
///
/// 状态在构造时就建好、条目留空：条目只能在渲染期推送（`set_items` 要 `Window`），而那
/// 时候不该再新建实体——见 [`SettingsView::sync_selects`]。
struct Selects {
    profile: Entity<SelectState<Vec<Choice>>>,
    model: Entity<SelectState<Vec<Choice>>>,
    small_profile: Entity<SelectState<Vec<Choice>>>,
    small_model: Entity<SelectState<Vec<Choice>>>,
    thinking_mode: Entity<SelectState<Vec<Choice>>>,
    effort: Entity<SelectState<Vec<Choice>>>,
    _subscriptions: Vec<Subscription>,
}

/// 打开着的 Provider 弹窗。
enum ProviderDialog {
    /// 配置或编辑一个 provider 预设。
    Config {
        provider: ProviderSpecDto,
        /// 命中已有 profile 时是「编辑」，否则是「配置」。
        existing_profile: Option<ProfileDto>,
        base_url: Entity<InputState>,
        api_key: Entity<InputState>,
        model_id: Entity<InputState>,
        /// 三个输入框的变更订阅；随这一轮弹窗一起丢，不挂在页面上。
        _subscriptions: Vec<Subscription>,
    },
    /// 移除确认。
    Remove {
        provider: Option<ProviderSpecDto>,
        profile: ProfileDto,
    },
}

pub struct SettingsView {
    api: Api,
    section: SettingsSection,
    /// 宿主配置视图；还没取到时为 `None`，页面画「加载设置...」。
    config: Option<ConfigViewResponseDto>,
    /// Provider 预设目录。
    catalog: Vec<ProviderSpecDto>,
    /// 扩展清单（插件分区）；服务端此刻看到的那一份。
    extensions: Vec<ExtensionStateDto>,
    /// 模型分区里那份待保存的选区。
    selection: ModelSelection,
    /// 权限分区里那份待保存的档位。
    ///
    /// 与 `config.approval_mode` 分开：这里是「改到一半」的值，点「保存权限」才写回。
    yolo_enabled: bool,
    loading: bool,
    operation: Option<Operation>,
    feedback: Option<Feedback>,
    dialog: Option<ProviderDialog>,
    selects: Selects,
    synced_items: SelectItems,
    synced_selection: SelectionSnapshot,
    /// thinking 表单里两个自由输入：模式之外的字段由它们承载，它们才是那两项的持有者。
    effort_input: Entity<InputState>,
    budget_input: Entity<InputState>,
    /// 已经推给输入框的那份表单；与它相同就不写第二遍（写会重置光标）。
    synced_form: ThinkingFormValue,
    sidebar_open: bool,
    visible: bool,
    load_task: Option<Task<()>>,
    operation_task: Option<Task<()>>,
    /// 两个 thinking 输入框与 Esc 拦截器的订阅；与视图同寿命。
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(api: Api, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let effort_input = cx.new(|cx| InputState::new(window, cx).placeholder("可选，例如 high"));
        let budget_input = cx.new(|cx| InputState::new(window, cx).placeholder("输入 Token 数"));
        let selects = Selects::build(window, cx);
        let view = cx.weak_entity();
        let subscriptions = vec![
            // 输入框的值参与提交（预算必填与否、按钮亮不亮），因此每次变化都要重画这一页。
            cx.subscribe_in(&effort_input, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&budget_input, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            // Esc 关弹窗：页面本身不取焦，快捷键走应用级拦截器（与两个弹窗同一套做法）。
            cx.intercept_keystrokes(move |event, _, cx| {
                if event.keystroke.key.as_str() != "escape" {
                    return;
                }
                let handled = view
                    .update(cx, |this, cx| {
                        // 拦截器与视图同寿命，而弹窗只在设置页里有意义：不在这一页时不接这
                        // 一下，否则会话页的 Esc 会被一个看不见的弹窗吃掉。
                        if !this.visible {
                            return false;
                        }
                        this.dismiss_dialog(cx)
                    })
                    .unwrap_or(false);
                if handled {
                    cx.stop_propagation();
                }
            }),
        ];
        Self {
            api,
            section: SettingsSection::Models,
            config: None,
            catalog: Vec::new(),
            extensions: Vec::new(),
            selection: ModelSelection::default(),
            yolo_enabled: false,
            loading: true,
            operation: None,
            feedback: None,
            dialog: None,
            selects,
            synced_items: SelectItems::default(),
            synced_selection: SelectionSnapshot::default(),
            effort_input,
            budget_input,
            synced_form: ThinkingFormValue::default(),
            sidebar_open: true,
            visible: false,
            load_task: None,
            operation_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// 侧边栏是否显示；外壳切换时告知。
    pub fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open == open {
            return;
        }
        self.sidebar_open = open;
        cx.notify();
    }

    /// 显示或隐藏本页；由外壳在切换主区域时告知。
    ///
    /// 每次显示都重取配置：别处（会话面板的模型按钮、磁盘上的 config.toml）都可能改过它，
    /// 进来时该显示的是此刻的真实值。
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.load(cx);
        }
        cx.notify();
    }

    /// 取一次配置视图、provider 目录与扩展清单。三个请求各报各的错，一个失败不遮住另一个
    /// （前端同判据）。
    fn load(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        self.loading = true;
        cx.notify();
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let config = api.config().await;
            let catalog = api.provider_catalog().await;
            let extensions = api.list_extensions().await;
            this.update(cx, |this, cx| {
                let mut errors: Vec<String> = Vec::new();
                match config {
                    Ok(config) => this.apply_config(config),
                    Err(error) => errors.push(format!("加载配置失败：{error}")),
                }
                match catalog {
                    Ok(catalog) => this.catalog = catalog.providers,
                    Err(error) => errors.push(format!("加载 Provider 列表失败：{error}")),
                }
                match extensions {
                    Ok(extensions) => this.extensions = extensions,
                    Err(error) => errors.push(format!("加载扩展列表失败：{error}")),
                }
                this.loading = false;
                this.feedback = if errors.is_empty() {
                    None
                } else {
                    Some(Feedback::Error(errors.join("\n")))
                };
                cx.notify();
            })
            .ok();
        }));
    }

    /// 装上配置视图：选区与权限档位都按它重置（前端 `applyConfig`）。
    fn apply_config(&mut self, config: ConfigViewResponseDto) {
        self.yolo_enabled = config.approval_mode == ApprovalModeDto::Yolo;
        self.selection = settings::model_selection_from_config(&config);
        self.config = Some(config);
    }

    /// 配置里某个 profile 的某个模型。
    fn model_of(&self, profile_name: &str, model_id: &str) -> Option<ModelDto> {
        self.config
            .as_ref()?
            .profiles
            .iter()
            .find(|profile| profile.name == profile_name)?
            .models
            .iter()
            .find(|model| model.id == model_id)
            .cloned()
    }

    /// 待保存选区里主模型的能力声明。
    fn selected_capability(&self) -> Option<ThinkingCapabilityDto> {
        self.model_of(&self.selection.profile_name, &self.selection.model_id)
            .and_then(|model| model.thinking_capability)
    }

    /// 换 profile：模型跟着挑一个，thinking 表单按新组合重新推导（前端 `handleSelectionChange`）。
    fn set_profile(&mut self, profile_name: String, cx: &mut Context<Self>) {
        let picked = {
            let Some(config) = self.config.as_ref() else {
                return;
            };
            let profile = config
                .profiles
                .iter()
                .find(|profile| profile.name == profile_name);
            settings::pick_model(profile, &self.selection.model_id)
        };
        self.selection.profile_name = profile_name;
        self.selection.model_id = picked;
        self.rederive_thinking_form();
        self.feedback = None;
        cx.notify();
    }

    fn set_model(&mut self, model_id: String, cx: &mut Context<Self>) {
        self.selection.model_id = model_id;
        self.rederive_thinking_form();
        self.feedback = None;
        cx.notify();
    }

    /// 换小模型 profile：小模型按它的第一个顶上，清空就是「不使用」（前端同口径）。
    fn set_small_profile(&mut self, profile_name: String, cx: &mut Context<Self>) {
        let picked = {
            let Some(config) = self.config.as_ref() else {
                return;
            };
            config
                .profiles
                .iter()
                .find(|profile| profile.name == profile_name)
                .and_then(|profile| profile.models.first())
                .map(|model| model.id.clone())
                .unwrap_or_default()
        };
        self.selection.small_profile_name = profile_name;
        self.selection.small_model_id = picked;
        self.feedback = None;
        cx.notify();
    }

    /// 换模型后重推 thinking 表单（前端 `deriveModelThinkingForm`）。
    fn rederive_thinking_form(&mut self) {
        let Some(config) = self.config.as_ref() else {
            return;
        };
        self.selection.thinking_form = settings::derive_model_thinking_form(
            config,
            &self.selection.profile_name,
            &self.selection.model_id,
        );
    }

    /// 表单值：模式取自选区，effort 与预算取自两个输入框。
    fn thinking_form(&self, cx: &Context<Self>) -> ThinkingFormValue {
        ThinkingFormValue {
            mode: self.selection.thinking_form.mode,
            effort: self.effort_input.read(cx).value().to_string(),
            budget_tokens: self.budget_input.read(cx).value().to_string(),
        }
    }

    /// 把 thinking 表单写进两个输入框；只在真的变了时写（`set_value` 会把光标打回开头）。
    fn sync_thinking_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = self.selection.thinking_form.clone();
        if self.synced_form == form {
            return;
        }
        if self.synced_form.effort != form.effort {
            self.effort_input.update(cx, |input, cx| {
                input.set_value(form.effort.clone(), window, cx)
            });
        }
        if self.synced_form.budget_tokens != form.budget_tokens {
            self.budget_input.update(cx, |input, cx| {
                input.set_value(form.budget_tokens.clone(), window, cx)
            });
        }
        self.synced_form = form;
    }

    /// 把六个下拉的条目与选中项对齐到当前配置。
    ///
    /// 推送条目要一个 `Window`，而配置是异步到手的，能拿到窗口的地方只有渲染期，所以对齐
    /// 摆在渲染的头一步做。这里只改已有实体的状态、不新建实体：换掉还挂在树上的那份选择器，
    /// 等于在别人读它的时候把它抽走。
    fn sync_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = Self::select_items(self.config.as_ref(), &self.selection);
        let snapshot = Self::selection_snapshot(&self.selection);
        let items_changed = self.synced_items != items;
        if !items_changed && self.synced_selection == snapshot {
            return;
        }
        let selects = &self.selects;
        if items_changed {
            push_items(&selects.profile, items.profile.clone(), window, cx);
            push_items(&selects.model, items.model.clone(), window, cx);
            push_items(
                &selects.small_profile,
                items.small_profile.clone(),
                window,
                cx,
            );
            push_items(&selects.small_model, items.small_model.clone(), window, cx);
            push_items(
                &selects.thinking_mode,
                items.thinking_mode.clone(),
                window,
                cx,
            );
            push_items(&selects.effort, items.effort.clone(), window, cx);
            self.synced_items = items;
        }
        set_selected(&selects.profile, &snapshot.profile, window, cx);
        set_selected(&selects.model, &snapshot.model, window, cx);
        set_selected(&selects.small_profile, &snapshot.small_profile, window, cx);
        set_selected(&selects.small_model, &snapshot.small_model, window, cx);
        set_selected(
            &selects.thinking_mode,
            mode_value(snapshot.thinking_mode),
            window,
            cx,
        );
        set_selected(&selects.effort, &snapshot.effort, window, cx);
        self.synced_selection = snapshot;
    }

    /// 按当前配置与选区算出六个下拉的条目。
    fn select_items(
        config: Option<&ConfigViewResponseDto>,
        selection: &ModelSelection,
    ) -> SelectItems {
        let profiles = config
            .map(|config| config.profiles.as_slice())
            .unwrap_or(&[]);
        let profile_choices: Vec<Choice> = profiles
            .iter()
            .map(|profile| Choice {
                label: profile_option_label(profile),
                value: profile.name.clone(),
            })
            .collect();

        let current = profiles
            .iter()
            .find(|profile| profile.name == selection.profile_name);
        let current_models = current.map(model_choices).unwrap_or_default();

        let mut small_profile_choices = vec![Choice {
            label: "不使用".into(),
            value: String::new(),
        }];
        small_profile_choices.extend(profile_choices.iter().cloned());

        let small_model_choices = profiles
            .iter()
            .find(|profile| profile.name == selection.small_profile_name)
            .map(model_choices)
            .unwrap_or_else(|| {
                vec![Choice {
                    label: "不使用".into(),
                    value: String::new(),
                }]
            });
        let capability = current
            .and_then(|profile| {
                profile
                    .models
                    .iter()
                    .find(|model| model.id == selection.model_id)
            })
            .and_then(|model| model.thinking_capability.as_ref());
        let thinking_mode = capability.map(thinking_mode_choices).unwrap_or_default();
        let effort = capability.map(effort_choices).unwrap_or_default();

        SelectItems {
            profile: profile_choices,
            model: current_models,
            small_profile: small_profile_choices,
            small_model: small_model_choices,
            thinking_mode,
            effort,
        }
    }

    /// 下拉此刻该显示的值。
    fn selection_snapshot(selection: &ModelSelection) -> SelectionSnapshot {
        SelectionSnapshot {
            profile: selection.profile_name.clone(),
            model: selection.model_id.clone(),
            small_profile: selection.small_profile_name.clone(),
            small_model: selection.small_model_id.clone(),
            thinking_mode: selection.thinking_form.mode,
            effort: selection.thinking_form.effort.clone(),
        }
    }

    /// 换 thinking 模式；离开「启用」时 effort 与预算一并清掉（前端同口径）。
    fn set_thinking_mode(&mut self, mode: ThinkingFormMode, cx: &mut Context<Self>) {
        self.selection.thinking_form.mode = mode;
        if mode != ThinkingFormMode::Enabled {
            self.selection.thinking_form.effort.clear();
            self.selection.thinking_form.budget_tokens.clear();
        }
        self.feedback = None;
        cx.notify();
    }

    /// 选努力层级：枚举与自由输入共用这一个入口，因此两个持有者一起写。
    fn set_effort(&mut self, effort: String, window: &mut Window, cx: &mut Context<Self>) {
        self.selection.thinking_form.effort = effort.clone();
        self.synced_form.effort = effort.clone();
        self.effort_input
            .update(cx, |input, cx| input.set_value(effort, window, cx));
        self.feedback = None;
        cx.notify();
    }

    /// 开始一次操作：清掉上一条反馈，记下正在跑的那一件。
    fn begin_operation(&mut self, operation: Operation, cx: &mut Context<Self>) {
        self.operation = Some(operation);
        self.feedback = None;
        cx.notify();
    }

    /// 操作收尾：放开按钮、写一句结论，并让外壳去刷新它关心的那一份状态。
    ///
    /// 结论收 [`Feedback`] 而不是字符串：扩展的重载错误落在失败侧，却与成功共用这条结果条。
    fn finish_operation(
        &mut self,
        feedback: Feedback,
        event: SettingsEvent,
        cx: &mut Context<Self>,
    ) {
        self.operation = None;
        self.feedback = Some(feedback);
        cx.emit(event);
        cx.notify();
    }

    /// 操作失败收尾：放开按钮并报错。
    fn fail_operation(&mut self, message: String, cx: &mut Context<Self>) {
        self.operation = None;
        self.feedback = Some(Feedback::Error(message));
        cx.notify();
    }

    /// 权限档位的线缆值。
    fn approval_mode(&self) -> ApprovalModeDto {
        if self.yolo_enabled {
            ApprovalModeDto::Yolo
        } else {
            ApprovalModeDto::Manual
        }
    }

    /// 选区请求：小模型两项取表单里的值，空串落成 `None`（前端 `|| undefined`）。
    fn selection_request(&self) -> UpdateActiveSelectionRequest {
        UpdateActiveSelectionRequest {
            active_profile: self.selection.profile_name.clone(),
            active_model: self.selection.model_id.clone(),
            active_small_profile: optional(&self.selection.small_profile_name),
            active_small_model: optional(&self.selection.small_model_id),
            approval_mode: self.approval_mode(),
        }
    }

    /// 保存模型：先写 thinking 选项，再写选区（前端 `handleSave` 同序）。
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.selection.profile_name.is_empty() || self.selection.model_id.is_empty() {
            return;
        }
        let form = self.thinking_form(cx);
        let capability = self.selected_capability();
        let request = match settings::thinking_form_to_request(
            &self.selection.profile_name,
            &self.selection.model_id,
            &form,
            capability.as_ref(),
        ) {
            Ok(request) => request,
            Err(message) => {
                // 校验失败是本地的：不发请求，也不进「正在保存」。
                self.feedback = Some(Feedback::Error(message));
                cx.notify();
                return;
            },
        };
        let selection = self.selection_request();
        let api = self.api.clone();
        self.begin_operation(Operation::Save, cx);
        self.operation_task = Some(cx.spawn(async move |this, cx| {
            let outcome = match api.update_model_options(&request).await {
                Ok(_) => api.update_active_selection(&selection).await.map(|_| ()),
                Err(error) => Err(error),
            };
            // 成败都重取：失败也可能是「服务端收下了但没回完」，界面该显示真实的那一份。
            let config = api.config().await.ok();
            this.update(cx, |this, cx| {
                if let Some(config) = config {
                    this.apply_config(config);
                }
                match outcome {
                    Ok(()) => this.finish_operation(
                        Feedback::Success("已保存".to_owned()),
                        SettingsEvent::ModelChanged,
                        cx,
                    ),
                    Err(error) => this.fail_operation(error.to_string(), cx),
                }
            })
            .ok();
        }));
    }

    /// 从磁盘重载配置。
    fn reload(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        self.begin_operation(Operation::Reload, cx);
        self.operation_task = Some(cx.spawn(async move |this, cx| {
            let outcome = api.reload_config().await.map(|_| ());
            let config = api.config().await.ok();
            this.update(cx, |this, cx| {
                if let Some(config) = config {
                    this.apply_config(config);
                }
                match outcome {
                    Ok(()) => this.finish_operation(
                        Feedback::Success("已从磁盘重载".to_owned()),
                        SettingsEvent::ModelChanged,
                        cx,
                    ),
                    Err(error) => this.fail_operation(error.to_string(), cx),
                }
            })
            .ok();
        }));
    }

    /// 测试当前主模型的连通性。
    fn test(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        self.begin_operation(Operation::Test, cx);
        self.operation_task = Some(cx.spawn(async move |this, cx| {
            let outcome = api.test_model().await;
            this.update(cx, |this, cx| {
                this.operation = None;
                this.feedback = Some(match outcome {
                    Ok(result) => Feedback::Test {
                        success: result.success,
                        message: result.message,
                    },
                    Err(error) => Feedback::Test {
                        success: false,
                        message: error.to_string(),
                    },
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// 把某个已配置的 profile 设为当前；模型取它的第一个（前端 `handleActivateProfile`）。
    fn activate_profile(&mut self, profile_name: String, cx: &mut Context<Self>) {
        let Some(model_id) = self
            .config
            .as_ref()
            .and_then(|config| {
                config
                    .profiles
                    .iter()
                    .find(|profile| profile.name == profile_name)
            })
            .and_then(|profile| profile.models.first())
            .map(|model| model.id.clone())
        else {
            return;
        };
        let request = UpdateActiveSelectionRequest {
            active_profile: profile_name.clone(),
            active_model: model_id,
            active_small_profile: optional(&self.selection.small_profile_name),
            active_small_model: optional(&self.selection.small_model_id),
            approval_mode: self.approval_mode(),
        };
        let api = self.api.clone();
        self.begin_operation(
            Operation::ActivateProfile {
                profile_name: profile_name.clone(),
            },
            cx,
        );
        self.operation_task = Some(cx.spawn(async move |this, cx| {
            let outcome = api.update_active_selection(&request).await;
            let config = api.config().await.ok();
            this.update(cx, |this, cx| {
                if let Some(config) = config {
                    this.apply_config(config);
                }
                match outcome {
                    Ok(response) => {
                        let message = match response.warning {
                            Some(warning) => format!("已切换到 {profile_name}；{warning}"),
                            None => format!("已切换到 {profile_name}"),
                        };
                        this.finish_operation(
                            Feedback::Success(message),
                            SettingsEvent::ModelChanged,
                            cx,
                        )
                    },
                    Err(error) => this.fail_operation(error.to_string(), cx),
                }
            })
            .ok();
        }));
    }

    /// 应用 provider 预设：写入或覆盖一个 profile，`activate` 为真时同时设为当前。
    fn apply_provider(&mut self, activate: bool, cx: &mut Context<Self>) {
        let Some(ProviderDialog::Config {
            provider,
            base_url,
            api_key,
            model_id,
            ..
        }) = &self.dialog
        else {
            return;
        };
        let base_url = base_url.read(cx).value().trim().to_owned();
        if base_url.is_empty() {
            return;
        }
        // 模型留空就落回 provider 默认（前端 `modelId.trim() || provider.defaultModel`）。
        let typed_model = model_id.read(cx).value().trim().to_owned();
        let model_id = if typed_model.is_empty() {
            provider.default_model.clone()
        } else {
            typed_model
        };
        let request = ApplyProviderPresetRequest {
            provider_id: provider.id.clone(),
            endpoint_id: None,
            profile_name: None,
            base_url: Some(base_url),
            api_key: optional(&api_key.read(cx).value()),
            model_id: Some(model_id),
            activate,
        };
        let api = self.api.clone();
        self.begin_operation(
            Operation::ApplyProvider {
                provider_id: provider.id.clone(),
            },
            cx,
        );
        self.operation_task = Some(cx.spawn(async move |this, cx| {
            let outcome = api.apply_provider_preset(&request).await;
            let config = api.config().await.ok();
            this.update(cx, |this, cx| {
                if let Some(config) = config {
                    this.apply_config(config);
                }
                match outcome {
                    Ok(response) => {
                        this.dialog = None;
                        let message = match response.warning {
                            Some(warning) => format!("已保存 {}；{warning}", response.profile_name),
                            None if response.activated => {
                                format!("已应用 {}", response.profile_name)
                            },
                            None => format!("已保存 {}", response.profile_name),
                        };
                        this.finish_operation(
                            Feedback::Success(message),
                            SettingsEvent::ModelChanged,
                            cx,
                        )
                    },
                    Err(error) => this.fail_operation(error.to_string(), cx),
                }
            })
            .ok();
        }));
    }

    /// 取消一个 provider 预设 profile。
    fn remove_provider(&mut self, profile_name: String, cx: &mut Context<Self>) {
        let request = RemoveProviderPresetRequest {
            profile_name: profile_name.clone(),
        };
        let api = self.api.clone();
        self.begin_operation(
            Operation::RemoveProfile {
                profile_name: profile_name.clone(),
            },
            cx,
        );
        self.operation_task = Some(cx.spawn(async move |this, cx| {
            let outcome = api.remove_provider_preset(&request).await;
            let config = api.config().await.ok();
            this.update(cx, |this, cx| {
                if let Some(config) = config {
                    this.apply_config(config);
                }
                match outcome {
                    Ok(response) => {
                        this.dialog = None;
                        let message = match response.warning {
                            Some(warning) => {
                                format!("已取消 {}；{warning}", response.removed_profile_name)
                            },
                            None => format!("已取消 {}", response.removed_profile_name),
                        };
                        this.finish_operation(
                            Feedback::Success(message),
                            SettingsEvent::ModelChanged,
                            cx,
                        )
                    },
                    Err(error) => this.fail_operation(error.to_string(), cx),
                }
            })
            .ok();
        }));
    }

    /// 重载扩展注册表：报出这一轮的装载错误，并重取清单（前端 `handleReloadExtensions`）。
    fn reload_extensions(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        self.begin_operation(Operation::ReloadExtensions, cx);
        self.operation_task = Some(cx.spawn(async move |this, cx| {
            let outcome = api.reload_extensions().await;
            let extensions = api.list_extensions().await.ok();
            this.update(cx, |this, cx| {
                if let Some(extensions) = extensions {
                    this.extensions = extensions;
                }
                match outcome {
                    Ok(response) => this.finish_operation(
                        reload_feedback("已重载扩展", &response.reload_errors),
                        SettingsEvent::ExtensionsChanged,
                        cx,
                    ),
                    Err(error) => this.fail_operation(error.to_string(), cx),
                }
            })
            .ok();
        }));
    }

    /// 启用或禁用单个扩展（前端 `handleToggleExtension`）。
    ///
    /// 清单成败都重取：请求失败也可能是服务端已经写下了配置、只是回执没走完，界面该显示服务端
    /// 此刻的那一份，而不是本地猜的那一份。
    fn set_extension_enabled(
        &mut self,
        extension_id: String,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let api = self.api.clone();
        self.begin_operation(Operation::SetExtension, cx);
        self.operation_task = Some(cx.spawn(async move |this, cx| {
            let outcome = api.set_extension_enabled(&extension_id, enabled).await;
            let extensions = api.list_extensions().await.ok();
            this.update(cx, |this, cx| {
                if let Some(extensions) = extensions {
                    this.extensions = extensions;
                }
                match outcome {
                    Ok(response) => {
                        let action = if enabled { "已启用" } else { "已禁用" };
                        this.finish_operation(
                            reload_feedback(
                                &format!("{action} {extension_id}"),
                                &response.reload_errors,
                            ),
                            SettingsEvent::ExtensionsChanged,
                            cx,
                        )
                    },
                    Err(error) => this.fail_operation(error.to_string(), cx),
                }
            })
            .ok();
        }));
    }

    /// 打开配置弹窗：Base URL 与模型取此刻能定的那个默认值（前端 `openProviderConfigDialog`）。
    fn open_config_dialog(
        &mut self,
        provider: ProviderSpecDto,
        existing_profile: Option<ProfileDto>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let base_url = dialog_default_base_url(&provider, existing_profile.as_ref());
        let model_id = dialog_default_model(
            &provider,
            existing_profile.as_ref(),
            &self.selection.model_id,
        );
        let key_placeholder = if existing_profile
            .as_ref()
            .is_some_and(|profile| profile.has_api_key)
        {
            "已配置，留空保留".to_owned()
        } else {
            match provider.api_key_env_vars.first() {
                Some(var) => format!("API Key 或 env:{var}"),
                None => "API Key".to_owned(),
            }
        };
        let base_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://api.example.com/v1")
                .default_value(base_url)
        });
        let api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(key_placeholder)
                .default_value("")
        });
        let model_id_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(provider.default_model.clone())
                .default_value(model_id)
        });
        // 三个输入框的当前值参与「保存」按钮的亮灭，每次变化都要重画这一页。
        let subscriptions: Vec<Subscription> = [&base_url_input, &api_key_input, &model_id_input]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |_, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        self.dialog = Some(ProviderDialog::Config {
            provider,
            existing_profile,
            base_url: base_url_input,
            api_key: api_key_input,
            model_id: model_id_input,
            _subscriptions: subscriptions,
        });
        self.feedback = None;
        cx.notify();
    }

    /// 打开移除确认。
    fn open_remove_dialog(
        &mut self,
        provider: Option<ProviderSpecDto>,
        profile: ProfileDto,
        cx: &mut Context<Self>,
    ) {
        self.dialog = Some(ProviderDialog::Remove { provider, profile });
        self.feedback = None;
        cx.notify();
    }

    /// 关掉弹窗；有操作在跑时不关（前端同样拒绝在提交中关闭）。返回是否真的关了。
    fn dismiss_dialog(&mut self, cx: &mut Context<Self>) -> bool {
        if self.operation.is_some() || self.dialog.is_none() {
            return false;
        }
        self.dialog = None;
        cx.notify();
        true
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 两步对齐都只在真正变了时动手，因此逐帧调用的代价只是两次比较。
        self.sync_selects(window, cx);
        self.sync_thinking_inputs(window, cx);

        let mut root = v_flex()
            .id("settings")
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(self.render_body(cx));
        if let Some(dialog) = self.render_dialog(cx) {
            root = root.child(dialog);
        }
        root
    }
}

impl SettingsView {
    /// 页头：收起侧边栏时的展开入口 + 分区名。
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut header = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_6()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border);
        if !self.sidebar_open {
            header = header.child(icon_button(
                "settings-expand-sidebar",
                IconName::Sidebar,
                cx,
                |_this: &mut SettingsView, cx| cx.emit(SettingsEvent::ToggleSidebar),
            ));
        }
        header
            .child(
                IconName::Settings
                    .element(Size::Small)
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().foreground)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("设置"),
            )
            .into_any_element()
    }

    /// 页面主体：左侧分区导航 + 右侧内容。
    fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.loading {
            return h_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("加载设置...")
                .into_any_element();
        }

        let mut content = v_flex()
            .min_w_0()
            .max_w(px(CONTENT_MAX_WIDTH))
            .gap_4()
            .child(self.render_section_title(cx));
        content = content.children(self.render_section(cx));
        if let Some(feedback) = self.render_feedback(cx) {
            content = content.child(feedback);
        }

        h_flex()
            .flex_1()
            .min_h_0()
            .gap_7()
            .px_6()
            .py_5()
            .child(self.render_nav(cx))
            .child(self.render_content_column(content, cx))
            .into_any_element()
    }

    /// 内容列：超宽时横向留白、超出视口时纵向滚动。
    fn render_content_column(
        &self,
        content: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id("settings-content")
            .flex_1()
            .min_w_0()
            .h_full()
            .justify_center()
            .overflow_y_scroll()
            .child(content)
            .text_color(cx.theme().foreground)
            .into_any_element()
    }

    /// 分区导航：当前分区铺底高亮。
    fn render_nav(&self, cx: &mut Context<Self>) -> AnyElement {
        let sections = [
            SettingsSection::Models,
            SettingsSection::Providers,
            SettingsSection::Permissions,
            SettingsSection::Plugins,
        ];
        let items: Vec<AnyElement> = sections
            .into_iter()
            .map(|section| {
                let active = self.section == section;
                h_flex()
                    .id(SharedString::from(format!("settings-nav-{section:?}")))
                    .items_center()
                    .gap_2()
                    .w_full()
                    .min_h(px(40.0))
                    .px_2()
                    .rounded(cx.theme().radius)
                    .text_sm()
                    .bg(if active {
                        cx.theme().list_active
                    } else {
                        cx.theme().transparent
                    })
                    .child(section_icon(section).element(Size::Small))
                    .child(section.label().to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.section = section;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .flex_shrink_0()
            .w(px(NAV_WIDTH))
            .gap_1()
            .children(items)
            .into_any_element()
    }

    /// 分区标题：图标 + 名字 + 说明。
    fn render_section_title(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        section_icon(self.section)
                            .element(Size::Small)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.section.label().to_string()),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.section.hint().to_string()),
            )
            .into_any_element()
    }

    /// 当前分区的全部内容块。
    fn render_section(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match self.section {
            SettingsSection::Models => self.render_models(cx),
            SettingsSection::Providers => self.render_providers(cx),
            SettingsSection::Permissions => self.render_permissions(cx),
            SettingsSection::Plugins => self.render_plugins(cx),
        }
    }

    /// 模型分区：两张摘要 + thinking 表单 + 四个选择行 + 两个只读行 + 动作行。
    fn render_models(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let profiles = self
            .config
            .as_ref()
            .map(|config| config.profiles.as_slice())
            .unwrap_or(&[]);
        let current_profile = profiles
            .iter()
            .find(|profile| profile.name == self.selection.profile_name);
        let small_profile = profiles
            .iter()
            .find(|profile| profile.name == self.selection.small_profile_name);
        let model = current_profile
            .and_then(|profile| {
                profile
                    .models
                    .iter()
                    .find(|model| model.id == self.selection.model_id)
            })
            .cloned();
        let capability = model
            .as_ref()
            .and_then(|model| model.thinking_capability.clone());
        let selects = &self.selects;
        let busy = self.operation.is_some();
        let saving = self.operation.as_ref() == Some(&Operation::Save);
        let reloading = self.operation.as_ref() == Some(&Operation::Reload);
        let testing = self.operation.as_ref() == Some(&Operation::Test);
        let has_selection =
            !self.selection.profile_name.is_empty() && !self.selection.model_id.is_empty();

        // 两张摘要并排：与前端 `md:grid-cols-2` 同一形态。
        let summaries = h_flex()
            .w_full()
            .child(summary_block(
                "主模型",
                current_profile
                    .map(|profile| settings::wire_format_label(profile.wire_format).to_owned())
                    .unwrap_or_else(|| "-".to_owned()),
                non_empty_or(&self.selection.model_id, "-"),
                non_empty_or(&self.selection.profile_name, "-"),
                false,
                cx,
            ))
            .child(summary_block(
                "小模型",
                small_profile
                    .map(|profile| settings::wire_format_label(profile.wire_format).to_owned())
                    .unwrap_or_else(|| "可选".to_owned()),
                non_empty_or(&self.selection.small_model_id, "未启用"),
                non_empty_or(&self.selection.small_profile_name, "不使用"),
                true,
                cx,
            ));

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(capability) = &capability {
            rows.extend(self.render_thinking_rows(capability, cx));
        }
        rows.push(panel_row(
            "Profile",
            current_profile
                .map(|profile| {
                    format!(
                        "{} · {}",
                        settings::wire_format_label(profile.wire_format),
                        settings::auth_scheme_label(profile.auth_scheme)
                    )
                })
                .unwrap_or_else(|| "-".to_owned()),
            select_control(&selects.profile, false, cx),
            !rows.is_empty(),
            cx,
        ));
        rows.push(panel_row(
            "Model",
            "当前对话默认模型".to_owned(),
            select_control(
                &selects.model,
                current_profile.map(|profile| profile.models.is_empty()) != Some(false),
                cx,
            ),
            true,
            cx,
        ));
        rows.push(panel_row(
            "Small Profile",
            "轻量任务模型配置".to_owned(),
            select_control(&selects.small_profile, false, cx),
            true,
            cx,
        ));
        rows.push(panel_row(
            "Small Model",
            small_profile
                .map(|profile| {
                    format!(
                        "{} · {}",
                        settings::wire_format_label(profile.wire_format),
                        settings::auth_scheme_label(profile.auth_scheme)
                    )
                })
                .unwrap_or_else(|| "未启用".to_owned()),
            select_control(
                &selects.small_model,
                small_profile.map(|profile| profile.models.is_empty()) != Some(false),
                cx,
            ),
            true,
            cx,
        ));
        rows.push(read_only_row(
            "Base URL",
            current_profile
                .map(|profile| profile.base_url.clone())
                .filter(|base_url| !base_url.is_empty())
                .unwrap_or_else(|| "-".to_owned()),
            cx,
        ));
        rows.push(read_only_row(
            "API Key",
            if current_profile.is_some_and(|profile| profile.has_api_key) {
                "已配置".to_owned()
            } else {
                "未配置".to_owned()
            },
            cx,
        ));

        vec![
            panel(
                std::iter::once(summaries.into_any_element())
                    .chain(rows)
                    .collect(),
                cx,
            ),
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("settings-reload")
                        .outline()
                        .label(if reloading {
                            "重载中..."
                        } else {
                            "从磁盘重载"
                        })
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                )
                .child(
                    Button::new("settings-test")
                        .outline()
                        .label(if testing {
                            "测试中..."
                        } else {
                            "测试连接"
                        })
                        .disabled(busy || !has_selection)
                        .on_click(cx.listener(|this, _, _, cx| this.test(cx))),
                )
                .child(
                    Button::new("settings-save")
                        .primary()
                        .label(if saving {
                            "保存中..."
                        } else {
                            "保存模型"
                        })
                        .disabled(busy || !has_selection)
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                )
                .into_any_element(),
        ]
    }

    /// thinking 三行：模式，以及「启用」时才出现的努力层级与预算（前端同判据）。
    fn render_thinking_rows(
        &self,
        capability: &ThinkingCapabilityDto,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let selects = &self.selects;
        let mode = self.selection.thinking_form.mode;
        let accepts_custom_effort = capability.allowed_effort.is_none();
        let has_effort = accepts_custom_effort
            || capability
                .allowed_effort
                .as_ref()
                .is_some_and(|efforts| !efforts.is_empty());
        let has_budget = capability.budget_min.is_some() || capability.budget_max.is_some();

        let mut rows = vec![panel_row(
            "Thinking",
            if settings::is_toggle_only_thinking(Some(capability)) {
                "仅开关".to_owned()
            } else {
                "思考与推理".to_owned()
            },
            select_control(&selects.thinking_mode, false, cx),
            true,
            cx,
        )];
        if mode != ThinkingFormMode::Enabled {
            return rows;
        }
        if has_effort {
            let control = if accepts_custom_effort {
                h_flex()
                    .w(px(SELECT_WIDTH))
                    .child(Input::new(&self.effort_input))
                    .into_any_element()
            } else {
                select_control(&selects.effort, false, cx)
            };
            rows.push(panel_row(
                "努力层级",
                "思考深度".to_owned(),
                control,
                true,
                cx,
            ));
        }
        if has_budget {
            let hint = [
                capability
                    .budget_min
                    .map(|min| format!("最小 {min}"))
                    .unwrap_or_default(),
                capability
                    .budget_max
                    .map(|max| format!("最大 {max}"))
                    .unwrap_or_default(),
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("，");
            rows.push(panel_row(
                "预算 Token",
                hint,
                h_flex()
                    .w(px(SELECT_WIDTH))
                    .child(Input::new(&self.budget_input))
                    .into_any_element(),
                true,
                cx,
            ));
        }
        rows
    }

    /// Providers 分区：已配置 Profiles 与 Provider Presets 两块面板。
    fn render_providers(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let profiles = self
            .config
            .as_ref()
            .map(|config| config.profiles.as_slice())
            .unwrap_or(&[]);
        let busy = self.operation.is_some();
        let reloading = self.operation.as_ref() == Some(&Operation::Reload);

        let mut configured: Vec<AnyElement> = vec![panel_header(
            "已配置 Profiles",
            format!("{} configured", profiles.len()),
            Some(
                Button::new("settings-providers-reload")
                    .outline()
                    .label(if reloading { "重载中..." } else { "重载" })
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
                    .into_any_element(),
            ),
            cx,
        )];
        if profiles.is_empty() {
            configured.push(
                div()
                    .w_full()
                    .px_4()
                    .py_6()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("暂无配置")
                    .into_any_element(),
            );
        }
        for profile in profiles {
            configured.push(self.render_configured_row(profile, cx));
        }

        let mut panels = vec![panel(configured, cx)];
        if !self.catalog.is_empty() {
            let mut presets: Vec<AnyElement> = vec![panel_header(
                "Provider Presets",
                format!("{} presets", self.catalog.len()),
                None,
                cx,
            )];
            for provider in &self.catalog {
                presets.push(self.render_preset_row(provider, profiles, cx));
            }
            panels.push(panel(presets, cx));
        }
        panels
    }

    /// 一行「已配置 Profiles」。
    fn render_configured_row(&self, profile: &ProfileDto, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.operation.is_some();
        let is_active = profile.name == self.selection.profile_name;
        let model_id = configured_model(profile, &self.selection.model_id);
        let activating = self.operation.as_ref()
            == Some(&Operation::ActivateProfile {
                profile_name: profile.name.clone(),
            });
        let removing = self.operation.as_ref()
            == Some(&Operation::RemoveProfile {
                profile_name: profile.name.clone(),
            });

        let mut actions = h_flex().flex_shrink_0().gap_2();
        if !is_active {
            let name = profile.name.clone();
            actions = actions.child(
                Button::new(SharedString::from(format!("activate-{}", profile.name)))
                    .outline()
                    .label(if activating {
                        "切换中..."
                    } else {
                        "设为当前"
                    })
                    .disabled(busy || model_id.is_none())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.activate_profile(name.clone(), cx)),
                    ),
            );
        }
        let remove_profile = profile.clone();
        let remove_provider = self
            .catalog
            .iter()
            .find(|provider| {
                settings::find_provider_profile(std::slice::from_ref(profile), provider).is_some()
            })
            .cloned();
        actions = actions.child(
            Button::new(SharedString::from(format!("remove-{}", profile.name)))
                .danger()
                .label(if removing { "移除中..." } else { "移除" })
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_remove_dialog(remove_provider.clone(), remove_profile.clone(), cx)
                })),
        );

        let mut row = divider_row(cx);
        row = row
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(profile.name.clone()),
                            )
                            .child(badge("当前", cx.theme().success, is_active)),
                    )
                    .child(provider_metadata(profile, cx)),
            )
            .child(
                v_flex()
                    .w(px(260.0))
                    .flex_shrink_0()
                    .gap_1()
                    .text_xs()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(model_id.unwrap_or("-").to_owned()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(non_empty_or(&profile.base_url, "-")),
                    )
                    .child(div().text_color(cx.theme().muted_foreground).child(
                        if profile.has_api_key {
                            "Key 已配置".to_owned()
                        } else {
                            "Key 未配置".to_owned()
                        },
                    )),
            )
            .child(actions);
        row.into_any_element()
    }

    /// 一行 Provider 预设。
    fn render_preset_row(
        &self,
        provider: &ProviderSpecDto,
        profiles: &[ProfileDto],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.operation.is_some();
        let profile = settings::find_provider_profile(profiles, provider);
        let model_id =
            profile.and_then(|profile| configured_model(profile, &self.selection.model_id));
        let is_configured = profile.is_some();
        let is_active = profile.is_some_and(|profile| profile.name == self.selection.profile_name);
        let applying = self.operation.as_ref()
            == Some(&Operation::ApplyProvider {
                provider_id: provider.id.clone(),
            });
        let activating = profile.is_some_and(|profile| {
            self.operation.as_ref()
                == Some(&Operation::ActivateProfile {
                    profile_name: profile.name.clone(),
                })
        });
        let removing = profile.is_some_and(|profile| {
            self.operation.as_ref()
                == Some(&Operation::RemoveProfile {
                    profile_name: profile.name.clone(),
                })
        });

        let mut actions = h_flex().flex_shrink_0().gap_2();
        if let Some(profile) = profile
            && !is_active
        {
            let name = profile.name.clone();
            actions = actions.child(
                Button::new(SharedString::from(format!(
                    "preset-activate-{}",
                    provider.id
                )))
                .outline()
                .label(if activating {
                    "切换中..."
                } else {
                    "设为当前"
                })
                .disabled(busy || model_id.is_none())
                .on_click(
                    cx.listener(move |this, _, _, cx| this.activate_profile(name.clone(), cx)),
                ),
            );
        }
        if let Some(profile) = profile {
            let remove_profile = profile.clone();
            let remove_provider = provider.clone();
            actions = actions.child(
                Button::new(SharedString::from(format!("preset-remove-{}", provider.id)))
                    .danger()
                    .label(if removing { "移除中..." } else { "移除" })
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_remove_dialog(
                            Some(remove_provider.clone()),
                            remove_profile.clone(),
                            cx,
                        )
                    })),
            );
        }
        let configure_provider = provider.clone();
        let configure_profile = profile.cloned();
        actions = actions.child(
            Button::new(SharedString::from(format!(
                "preset-configure-{}",
                provider.id
            )))
            .outline()
            .label(if applying {
                "保存中..."
            } else if is_configured {
                "编辑"
            } else {
                "配置"
            })
            .disabled(busy)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_config_dialog(
                    configure_provider.clone(),
                    configure_profile.clone(),
                    window,
                    cx,
                )
            })),
        );

        let mut left = v_flex()
            .flex_1()
            .min_w_0()
            .child(
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(provider.display_name.clone()),
                    )
                    .child(badge(
                        if is_active {
                            "当前"
                        } else if is_configured {
                            "已配置"
                        } else {
                            "未配置"
                        },
                        if is_active {
                            cx.theme().success
                        } else if is_configured {
                            cx.theme().primary
                        } else {
                            cx.theme().muted_foreground
                        },
                        true,
                    )),
            )
            .child(provider_metadata_of(
                &provider.provider_kind,
                provider.wire_format,
                provider.auth_scheme,
                cx,
            ));
        let labels = capability_labels(provider);
        if !labels.is_empty() {
            left = left.child(
                h_flex()
                    .mt_2()
                    .flex_wrap()
                    .gap_1()
                    .children(labels.into_iter().map(|label| pill(label, cx))),
            );
        }

        let key_hint = preset_key_hint(profile, provider);
        divider_row(cx)
            .child(left)
            .child(
                v_flex()
                    .w(px(260.0))
                    .flex_shrink_0()
                    .gap_1()
                    .text_xs()
                    .child(div().min_w_0().truncate().child(match model_id {
                        Some(model_id) => model_id.to_owned(),
                        None => provider.default_model.clone(),
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(displayed_base_url(profile, provider)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(key_hint),
                    ),
            )
            .child(actions)
            .into_any_element()
    }

    /// 权限分区：两个档位 + 保存。
    fn render_permissions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let busy = self.operation.is_some();
        let saving = self.operation.as_ref() == Some(&Operation::Save);
        let options = [
            ("手动确认", "工具调用前请求批准", false),
            ("完全访问", "自动批准工具调用", true),
        ];
        let rows: Vec<AnyElement> = options
            .into_iter()
            .enumerate()
            .map(|(index, (title, hint, yolo))| {
                let checked = self.yolo_enabled == yolo;
                let mut row = h_flex()
                    .id(SharedString::from(format!("approval-{yolo}")))
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .px_4()
                    .py_3();
                if index > 0 {
                    row = row.border_t_1().border_color(cx.theme().border);
                }
                row.bg(if checked {
                    cx.theme().secondary_hover
                } else {
                    cx.theme().transparent
                })
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                        .child(
                            div()
                                .mt(px(2.0))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(hint),
                        ),
                )
                .child(radio_marker(checked, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.yolo_enabled = yolo;
                    this.feedback = None;
                    cx.notify();
                }))
                .into_any_element()
            })
            .collect();

        vec![
            panel(rows, cx),
            h_flex()
                .justify_end()
                .child(
                    Button::new("settings-save-permissions")
                        .primary()
                        .label(if saving {
                            "保存中..."
                        } else {
                            "保存权限"
                        })
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                )
                .into_any_element(),
        ]
    }

    /// 插件分区：一张扩展清单面板，标题行挂着计数与重载按钮。
    ///
    /// 前端那一页把「总数 / 已启用 / 已加载」铺成三张卡片；设置页的面板本来就是「标题 + 计数 +
    /// 动作」这一形态，再插三个卡片块就是重复，因此收成标题的副标题。
    fn render_plugins(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let busy = self.operation.is_some();
        let reloading = self.operation.as_ref() == Some(&Operation::ReloadExtensions);
        let enabled = self
            .extensions
            .iter()
            .filter(|extension| extension.enabled)
            .count();
        let loaded = self
            .extensions
            .iter()
            .filter(|extension| extension.loaded)
            .count();
        let mut rows: Vec<AnyElement> = vec![panel_header(
            "扩展",
            format!(
                "{} 个 · {enabled} 已启用 · {loaded} 已加载",
                self.extensions.len()
            ),
            Some(
                Button::new("settings-plugins-reload")
                    .outline()
                    .label(if reloading {
                        "重载中..."
                    } else {
                        "重载插件"
                    })
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.reload_extensions(cx)))
                    .into_any_element(),
            ),
            cx,
        )];
        if self.extensions.is_empty() {
            rows.push(
                div()
                    .w_full()
                    .px_4()
                    .py_6()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("暂无插件")
                    .into_any_element(),
            );
        }
        for extension in &self.extensions {
            rows.push(self.render_extension_row(extension, cx));
        }
        vec![panel(rows, cx)]
    }

    /// 一行扩展：左边是身份、来源与声明，右边是启用开关（前端插件页的卡片同形）。
    fn render_extension_row(
        &self,
        extension: &ExtensionStateDto,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let status = settings::extension_status(extension);
        let mut left = v_flex().flex_1().min_w_0().child(
            h_flex()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(extension.extension_id.clone()),
                )
                .child(badge(
                    status.label(),
                    extension_status_color(status, cx),
                    true,
                )),
        );
        left = left.child(extension_metadata(extension, cx));
        let blocked_reasons = extension
            .declaration
            .as_ref()
            .map(|declaration| declaration.blocked_reasons.as_slice())
            .unwrap_or(&[]);
        for reason in blocked_reasons {
            left = left.child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(settings::blocked_reason_label(reason)),
            );
        }
        if let Some(error) = extension
            .diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.last_error.clone())
        {
            left = left.child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error),
            );
        }

        let extension_id = extension.extension_id.clone();
        let enabled = extension.enabled;
        divider_row(cx)
            .child(left)
            .child(
                Checkbox::new(SharedString::from(format!("plugin-toggle-{extension_id}")))
                    .checked(enabled)
                    .label(if enabled { "启用" } else { "禁用" })
                    .disabled(self.operation.is_some())
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set_extension_enabled(extension_id.clone(), *checked, cx);
                    })),
            )
            .into_any_element()
    }

    /// 结果条；没有反馈时返回 `None`。
    fn render_feedback(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (success, message) = match self.feedback.as_ref()? {
            Feedback::Success(message) => (true, message.clone()),
            Feedback::Error(message) => (false, message.clone()),
            Feedback::Test { success, message } => (
                *success,
                format!(
                    "{}：{message}",
                    if *success {
                        "连接成功"
                    } else {
                        "连接失败"
                    }
                ),
            ),
        };
        let color = if success {
            cx.theme().success
        } else {
            cx.theme().danger
        };
        Some(
            div()
                .w_full()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(color.opacity(0.3))
                .bg(color.opacity(0.15))
                .px_4()
                .py_3()
                .text_sm()
                .text_color(color)
                .child(message)
                .into_any_element(),
        )
    }

    /// 打开着的弹窗；没有时返回 `None`。
    fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.dialog.as_ref()? {
            ProviderDialog::Config {
                provider,
                existing_profile,
                base_url,
                api_key,
                model_id,
                ..
            } => {
                let applying = self.operation.as_ref()
                    == Some(&Operation::ApplyProvider {
                        provider_id: provider.id.clone(),
                    });
                let can_submit = !applying && !base_url.read(cx).value().trim().is_empty();
                let has_key = existing_profile
                    .as_ref()
                    .is_some_and(|profile| profile.has_api_key);

                let mut card = v_flex()
                    .gap_4()
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .child(format!(
                                        "{} {}",
                                        if existing_profile.is_some() {
                                            "编辑"
                                        } else {
                                            "配置"
                                        },
                                        provider.display_name
                                    )),
                            )
                            .child(icon_button(
                                "provider-config-close",
                                IconName::Close,
                                cx,
                                |this: &mut SettingsView, cx| {
                                    this.dismiss_dialog(cx);
                                },
                            )),
                    )
                    .child(dialog_field(
                        "Base URL",
                        Input::new(base_url).disabled(applying),
                        cx,
                    ))
                    .child(dialog_field(
                        "API Key",
                        Input::new(api_key).disabled(applying),
                        cx,
                    ));
                if has_key {
                    card = card.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("已保存的 Key 不会显示。"),
                    );
                }
                card = card.child(dialog_field(
                    "Model",
                    Input::new(model_id).disabled(applying),
                    cx,
                ));

                let mut actions = h_flex().justify_end().gap_2();
                actions = actions.child(
                    Button::new("provider-config-cancel")
                        .outline()
                        .label("取消")
                        .disabled(applying)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.dismiss_dialog(cx);
                        })),
                );
                actions = actions.child(
                    Button::new("provider-config-save")
                        .outline()
                        .label("仅保存")
                        .disabled(!can_submit)
                        .on_click(cx.listener(|this, _, _, cx| this.apply_provider(false, cx))),
                );
                actions = actions.child(
                    Button::new("provider-config-apply")
                        .primary()
                        .label(if applying {
                            "保存中..."
                        } else {
                            "保存并使用"
                        })
                        .disabled(!can_submit)
                        .on_click(cx.listener(|this, _, _, cx| this.apply_provider(true, cx))),
                );
                card = card.child(actions);
                Some(self.render_overlay(CONFIG_DIALOG_WIDTH, card.into_any_element(), cx))
            },
            ProviderDialog::Remove { provider, profile } => {
                let removing = self.operation.as_ref()
                    == Some(&Operation::RemoveProfile {
                        profile_name: profile.name.clone(),
                    });
                let name = provider
                    .as_ref()
                    .map(|provider| provider.display_name.clone())
                    .unwrap_or_else(|| profile.name.clone());

                let mut card = v_flex()
                    .gap_4()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child(format!("移除 {name} 配置")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "这会删除 {} 的 Base URL、API Key 和模型配置。",
                                profile.name
                            )),
                    );
                if profile.name == self.selection.profile_name {
                    card = card.child(
                        div()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().warning.opacity(0.3))
                            .bg(cx.theme().warning.opacity(0.15))
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(cx.theme().warning)
                            .child("当前正在使用这个 Provider，移除后会切换到其他可用配置。"),
                    );
                }
                let remove_name = profile.name.clone();
                card = card.child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("provider-remove-cancel")
                                .outline()
                                .label("返回")
                                .disabled(removing)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dismiss_dialog(cx);
                                })),
                        )
                        .child(
                            Button::new("provider-remove-confirm")
                                .danger()
                                .label(if removing {
                                    "移除中..."
                                } else {
                                    "移除配置"
                                })
                                .disabled(removing)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_provider(remove_name.clone(), cx)
                                })),
                        ),
                );
                Some(self.render_overlay(REMOVE_DIALOG_WIDTH, card.into_any_element(), cx))
            },
        }
    }

    /// 弹窗的遮罩与卡片：点遮罩收起，点卡片不收起。
    fn render_overlay(&self, width: f32, card: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("settings-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(OVERLAY_PADDING))
            .bg(cx.theme().overlay)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.dismiss_dialog(cx);
                }),
            )
            .child(
                div()
                    .id("settings-dialog")
                    .w_full()
                    .max_w(px(width))
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().popover)
                    .p_6()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| cx.stop_propagation()),
                    )
                    .child(card),
            )
            .into_any_element()
    }
}

impl Selects {
    /// 建六个空下拉并挂上各自的确认回调。
    ///
    /// 条目由 [`SettingsView::sync_selects`] 在渲染期推入；这里给空表，表单也就没有可选项，
    /// 与「配置还没到手」是同一件事。
    fn build(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let profile = cx.new(|cx| SelectState::new(Vec::<Choice>::new(), None, window, cx));
        let model = cx.new(|cx| SelectState::new(Vec::<Choice>::new(), None, window, cx));
        let small_profile = cx.new(|cx| SelectState::new(Vec::<Choice>::new(), None, window, cx));
        let small_model = cx.new(|cx| SelectState::new(Vec::<Choice>::new(), None, window, cx));
        let thinking_mode = cx.new(|cx| SelectState::new(Vec::<Choice>::new(), None, window, cx));
        let effort = cx.new(|cx| SelectState::new(Vec::<Choice>::new(), None, window, cx));

        let subscriptions = vec![
            cx.subscribe_in(
                &profile,
                window,
                |this, _, event: &SelectEvent<Vec<Choice>>, _, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.set_profile(value.clone(), cx);
                    }
                },
            ),
            cx.subscribe_in(
                &model,
                window,
                |this, _, event: &SelectEvent<Vec<Choice>>, _, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.set_model(value.clone(), cx);
                    }
                },
            ),
            cx.subscribe_in(
                &small_profile,
                window,
                |this, _, event: &SelectEvent<Vec<Choice>>, _, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.set_small_profile(value.clone(), cx);
                    }
                },
            ),
            cx.subscribe_in(
                &small_model,
                window,
                |this, _, event: &SelectEvent<Vec<Choice>>, _, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.selection.small_model_id = value.clone();
                        this.feedback = None;
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &thinking_mode,
                window,
                |this, _, event: &SelectEvent<Vec<Choice>>, _, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.set_thinking_mode(mode_from_value(value), cx);
                    }
                },
            ),
            cx.subscribe_in(
                &effort,
                window,
                |this, _, event: &SelectEvent<Vec<Choice>>, window, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.set_effort(value.clone(), window, cx);
                    }
                },
            ),
        ];
        Selects {
            profile,
            model,
            small_profile,
            small_model,
            thinking_mode,
            effort,
            _subscriptions: subscriptions,
        }
    }
}

/// 把某个下拉的条目整套换掉。
fn push_items(
    state: &Entity<SelectState<Vec<Choice>>>,
    items: Vec<Choice>,
    window: &mut Window,
    cx: &mut Context<SettingsView>,
) {
    state.update(cx, |state, cx| state.set_items(items, window, cx));
}

/// 空串当「没有」：表单里清空小模型就是「不使用」，请求里必须落成 `None`。
fn optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// 空串换成占位文案。
fn non_empty_or(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

/// 扩展操作的结论：服务端报了装载错误时按失败着色——前端把 `reloadErrors` 单独放进错误条，
/// 这里两者共用同一条结果条，靠颜色区分。
fn reload_feedback(prefix: &str, reload_errors: &[String]) -> Feedback {
    if reload_errors.is_empty() {
        Feedback::Success(prefix.to_owned())
    } else {
        Feedback::Error(format!("{prefix}；{}", reload_errors.join("; ")))
    }
}

/// 扩展状态的颜色。
///
/// 前端的 `statusClass` 把「依赖阻塞」也算进成功色；这里按语义给警告色——文案说「阻塞」而颜色
/// 说「一切正常」是自相矛盾的。
fn extension_status_color(
    status: settings::ExtensionStatus,
    cx: &mut Context<SettingsView>,
) -> Hsla {
    match status {
        settings::ExtensionStatus::Loaded => cx.theme().success,
        settings::ExtensionStatus::Disabled => cx.theme().muted_foreground,
        settings::ExtensionStatus::Blocked | settings::ExtensionStatus::Unloaded => {
            cx.theme().warning
        },
    }
}

/// 一行扩展的来源、能力与声明行（前端插件页卡片里的那几行小字）。
fn extension_metadata(extension: &ExtensionStateDto, cx: &mut Context<SettingsView>) -> AnyElement {
    let declaration = extension.declaration.as_ref();
    let capabilities = declaration
        .map(|declaration| declaration.capabilities.as_slice())
        .unwrap_or(&[]);
    let mut summary = settings::extension_source_label(extension.source).to_owned();
    if !capabilities.is_empty() {
        let names: Vec<String> = capabilities.iter().map(wire_value).collect();
        summary.push_str(&format!(" · {}", names.join(", ")));
    }

    let mut metadata = div()
        .mt_1()
        .min_w_0()
        .truncate()
        .text_size(px(11.0))
        .text_color(cx.theme().muted_foreground)
        .child(summary);
    if let Some(declaration) = declaration {
        metadata = metadata
            .children(declaration_line(
                "提供服务",
                declaration.services.join(", "),
                cx,
            ))
            .children(declaration_line(
                "依赖",
                settings::extension_dependencies_label(&declaration.dependencies),
                cx,
            ))
            .children(declaration_line(
                "可调用",
                declaration.service_permissions.join(", "),
                cx,
            ));
    }
    metadata.into_any_element()
}

/// 声明里的一行「标签：值」；值为空时什么都不画。
fn declaration_line(
    label: &str,
    value: String,
    cx: &mut Context<SettingsView>,
) -> Option<AnyElement> {
    if value.is_empty() {
        return None;
    }
    Some(
        div()
            .mt_1()
            .min_w_0()
            .truncate()
            .text_size(px(11.0))
            .text_color(cx.theme().muted_foreground)
            .child(format!("{label}：{value}"))
            .into_any_element(),
    )
}

/// 线缆枚举的展示值。
///
/// 这些 DTO 的 `Serialize` 就是它们的线缆拼写，因此直接取序列化结果当文案，而不是再手抄一份
/// 映射表去跟协议对号——那份表迟早与协议脱节。取不到时给空串：它只进展示，不参与任何判定。
fn wire_value<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        _ => String::new(),
    }
}

/// 分区图标，与前端 `SETTINGS_NAV_ITEMS` 的 icon 一致；插件沿用前端插件入口的 `plug`，
/// 与 Providers 那一枚同形（前端本来也是同一枚）。
fn section_icon(section: SettingsSection) -> IconName {
    match section {
        SettingsSection::Models => IconName::Settings,
        SettingsSection::Providers => IconName::Plug,
        SettingsSection::Permissions => IconName::Shield,
        SettingsSection::Plugins => IconName::Plug,
    }
}

/// 把某个选择器的选中项改成 `value`；找不到这个值就等于没有选中。
fn set_selected(
    state: &Entity<SelectState<Vec<Choice>>>,
    value: &str,
    window: &mut Window,
    cx: &mut Context<SettingsView>,
) {
    let value = value.to_owned();
    state.update(cx, |state, cx| state.set_selected_value(&value, window, cx));
}

/// 一个 profile 的全部模型做成下拉条目。
fn model_choices(profile: &ProfileDto) -> Vec<Choice> {
    profile
        .models
        .iter()
        .map(|model| Choice {
            label: model.id.clone().into(),
            value: model.id.clone(),
        })
        .collect()
}

/// profile 下拉里的一项：名字加线缆格式，与前端 option 文案一致。
fn profile_option_label(profile: &ProfileDto) -> SharedString {
    format!(
        "{} · {}",
        profile.name,
        settings::wire_format_label(profile.wire_format)
    )
    .into()
}

/// thinking 模式的下拉条目；「关闭」只在模型允许时为选项。
fn thinking_mode_choices(capability: &ThinkingCapabilityDto) -> Vec<Choice> {
    [
        (ThinkingFormMode::Default, "使用模型默认值"),
        (ThinkingFormMode::Enabled, "启用"),
        (ThinkingFormMode::Disabled, "关闭"),
    ]
    .into_iter()
    .filter(|(mode, _)| *mode != ThinkingFormMode::Disabled || capability.can_disable)
    .map(|(mode, label)| Choice {
        label: label.into(),
        value: mode_value(mode).to_owned(),
    })
    .collect()
}

/// 努力层级的下拉条目：空串那一项就是「不填」。
fn effort_choices(capability: &ThinkingCapabilityDto) -> Vec<Choice> {
    let mut choices = vec![Choice {
        label: "请选择".into(),
        value: String::new(),
    }];
    if let Some(allowed) = &capability.allowed_effort {
        choices.extend(allowed.iter().map(|effort| Choice {
            label: settings::effort_label(effort).to_owned().into(),
            value: effort.clone(),
        }));
    }
    choices
}

/// thinking 模式的线缆值。
fn mode_value(mode: ThinkingFormMode) -> &'static str {
    match mode {
        ThinkingFormMode::Default => "default",
        ThinkingFormMode::Enabled => "enabled",
        ThinkingFormMode::Disabled => "disabled",
    }
}

/// 线缆值还原成模式；认不出来的一律当默认。
fn mode_from_value(value: &str) -> ThinkingFormMode {
    match value {
        "enabled" => ThinkingFormMode::Enabled,
        "disabled" => ThinkingFormMode::Disabled,
        _ => ThinkingFormMode::Default,
    }
}

/// profile 的当前模型：待保存选区命中就用它，否则用第一个（前端 `configuredModel`）。
fn configured_model<'a>(profile: &'a ProfileDto, selected_model_id: &str) -> Option<&'a str> {
    profile
        .models
        .iter()
        .find(|model| model.id == selected_model_id)
        .or_else(|| profile.models.first())
        .map(|model| model.id.as_str())
}

/// 预设行上显示的 Base URL：已配置的优先，其次默认 endpoint 的 URL，再次 endpoint 的标签。
fn displayed_base_url(profile: Option<&ProfileDto>, provider: &ProviderSpecDto) -> String {
    if let Some(base_url) = profile.map(|profile| profile.base_url.as_str())
        && !base_url.is_empty()
    {
        return base_url.to_owned();
    }
    provider
        .endpoints
        .iter()
        .find(|endpoint| endpoint.is_default)
        .and_then(|endpoint| endpoint.base_url.clone().or(Some(endpoint.label.clone())))
        .unwrap_or_else(|| "-".to_owned())
}

/// 预设行的 Key 提示：已配置的 profile 说「已配置」，否则退回环境变量名。
fn preset_key_hint(profile: Option<&ProfileDto>, provider: &ProviderSpecDto) -> String {
    if profile.is_some_and(|profile| profile.has_api_key) {
        return "Key 已配置".to_owned();
    }
    match provider.api_key_env_vars.first() {
        Some(var) => format!("Key env:{var}"),
        None => "Key 未配置".to_owned(),
    }
}

/// 预设行的能力标签（前端 `capabilityLabels`）。
fn capability_labels(provider: &ProviderSpecDto) -> Vec<&'static str> {
    [
        (provider.capabilities.prompt_cache_key, "Cache key"),
        (provider.capabilities.stream_usage, "Stream usage"),
        (provider.capabilities.reasoning_effort, "Reasoning"),
        (provider.capabilities.strict_tool_use, "Strict tools"),
    ]
    .into_iter()
    .filter_map(|(enabled, label)| enabled.then_some(label))
    .collect()
}

/// 配置弹窗预填的 Base URL：已有 profile 的优先，其次默认 endpoint。
fn dialog_default_base_url(provider: &ProviderSpecDto, existing: Option<&ProfileDto>) -> String {
    existing
        .map(|profile| profile.base_url.clone())
        .filter(|base_url| !base_url.is_empty())
        .or_else(|| {
            provider
                .endpoints
                .iter()
                .find(|endpoint| endpoint.is_default)
                .and_then(|endpoint| endpoint.base_url.clone())
        })
        .unwrap_or_default()
}

/// 配置弹窗预填的模型：命中待保存选区就用它，其次已有 profile 的第一个，最后 provider 的默认。
fn dialog_default_model(
    provider: &ProviderSpecDto,
    existing: Option<&ProfileDto>,
    selected_model_id: &str,
) -> String {
    existing
        .and_then(|profile| {
            profile
                .models
                .iter()
                .find(|model| model.id == selected_model_id)
                .or_else(|| profile.models.first())
        })
        .map(|model| model.id.clone())
        .unwrap_or_else(|| provider.default_model.clone())
}

/// 两块面板共用的外壳：圆角、描边、裁掉溢出的子元素。
fn panel(children: Vec<AnyElement>, cx: &mut Context<SettingsView>) -> AnyElement {
    v_flex()
        .w_full()
        .overflow_hidden()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
        .children(children)
        .into_any_element()
}

/// 面板的标题行：名字、计数，右侧可挂一个动作。
fn panel_header(
    title: &str,
    subtitle: String,
    action: Option<AnyElement>,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    let mut header = h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_3()
        .child(
            v_flex()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(subtitle),
                ),
        );
    if let Some(action) = action {
        header = header.child(action);
    }
    header.into_any_element()
}

/// 面板里的一行：左标签 + 说明，右侧控件；`divider` 为真时画上分隔线。
fn panel_row(
    label: &str,
    hint: String,
    control: AnyElement,
    divider: bool,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    let mut row = h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_3()
        .min_w_0();
    if divider {
        row = row.border_t_1().border_color(cx.theme().border);
    }
    let mut left = v_flex().min_w_0().child(
        div()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .child(label.to_string()),
    );
    if !hint.is_empty() {
        left = left.child(
            div()
                .mt(px(2.0))
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(hint),
        );
    }
    row.child(left).child(control).into_any_element()
}

/// 只读的一行：右值可能很长，让它自己换行。
fn read_only_row(label: &str, value: String, cx: &mut Context<SettingsView>) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_3()
        .min_w_0()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label.to_string()),
        )
        .child(div().min_w_0().text_sm().child(value))
        .into_any_element()
}

/// 模型分区顶部的一张摘要。
fn summary_block(
    label: &str,
    badge_text: String,
    model: String,
    profile: String,
    divided: bool,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    let mut block = v_flex().flex_1().min_w_0().px_4().py_3();
    if divided {
        block = block.border_l_1().border_color(cx.theme().border);
    }
    block
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().muted_foreground)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(cx.theme().border)
                        .px_2()
                        .py(px(2.0))
                        .text_size(px(11.0))
                        .text_color(cx.theme().muted_foreground)
                        .child(badge_text),
                ),
        )
        .child(
            div()
                .mt_2()
                .min_w_0()
                .truncate()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(model),
        )
        .child(
            div()
                .mt_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(profile),
        )
        .into_any_element()
}

/// 面板里一行带分隔线的行外壳：左侧内容自适应、右侧动作不缩。
///
/// provider 与扩展两类行共用它：两者的形状本来就一样——面板里一块一行，块顶换分隔线。
fn divider_row(cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .px_4()
        .py_3()
        .min_w_0()
        .border_t_1()
        .border_color(cx.theme().border)
}

/// provider 的种类、线缆格式与认证方式；三者都来自 profile。
fn provider_metadata(profile: &ProfileDto, cx: &mut Context<SettingsView>) -> AnyElement {
    provider_metadata_of(
        &profile.provider_kind,
        profile.wire_format,
        profile.auth_scheme,
        cx,
    )
}

/// provider 的种类、线缆格式与认证方式。
fn provider_metadata_of(
    provider_kind: &str,
    wire_format: astrcode_protocol::wire::ProviderWireFormatDto,
    auth_scheme: astrcode_protocol::wire::ProviderAuthSchemeDto,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    div()
        .mt_1()
        .min_w_0()
        .truncate()
        .text_size(px(11.0))
        .text_color(cx.theme().muted_foreground)
        .child(format!(
            "{provider_kind} · {} · {}",
            settings::wire_format_label(wire_format),
            settings::auth_scheme_label(auth_scheme)
        ))
        .into_any_element()
}

/// 一枚小徽标；`shown` 为假时什么都不画（用来省掉「当前」这种条件标记）。
fn badge(text: &str, color: Hsla, shown: bool) -> AnyElement {
    let mut element = div()
        .flex_shrink_0()
        .rounded(px(6.0))
        .px_2()
        .py(px(1.0))
        .text_size(px(11.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color);
    if !shown {
        return div().into_any_element();
    }
    element = element.bg(color.opacity(0.15));
    element.child(text.to_string()).into_any_element()
}

/// 能力标签那样的小胶囊。
fn pill(text: &str, cx: &mut Context<SettingsView>) -> AnyElement {
    div()
        .rounded(px(6.0))
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .px_2()
        .text_size(px(11.0))
        .text_color(cx.theme().muted_foreground)
        .child(text.to_string())
        .into_any_element()
}

/// 一个下拉控件；宽度统一，免得右侧参差。
fn select_control(
    state: &Entity<SelectState<Vec<Choice>>>,
    disabled: bool,
    _cx: &mut Context<SettingsView>,
) -> AnyElement {
    h_flex()
        .w(px(SELECT_WIDTH))
        .flex_shrink_0()
        .child(Select::new(state).disabled(disabled).w_full())
        .into_any_element()
}

/// 单选钮：选中铺主色，未选中只留描边（gpui 这版没有原生的 radio 元素）。
fn radio_marker(checked: bool, cx: &mut Context<SettingsView>) -> AnyElement {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(16.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(if checked {
            cx.theme().primary
        } else {
            cx.theme().border
        })
        .child(div().size(px(8.0)).rounded(px(4.0)).bg(if checked {
            cx.theme().primary
        } else {
            cx.theme().transparent
        }))
        .into_any_element()
}

/// 弹窗里的一格：标签在上，控件在下。
fn dialog_field(
    label: &str,
    control: impl IntoElement,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    v_flex()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(label.to_string()),
        )
        .child(control)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::{
        http::{ProviderEndpointPresetDto, ProviderSpecCapabilitiesDto},
        wire::{ProviderAuthSchemeDto, ProviderWireFormatDto},
    };

    use super::*;

    fn capabilities(
        prompt_cache_key: bool,
        stream_usage: bool,
        reasoning_effort: bool,
        strict_tool_use: bool,
    ) -> ProviderSpecCapabilitiesDto {
        ProviderSpecCapabilitiesDto {
            prompt_cache_key,
            stream_usage,
            reasoning_effort,
            strict_tool_use,
        }
    }

    fn endpoint(
        label: &str,
        base_url: Option<&str>,
        is_default: bool,
    ) -> ProviderEndpointPresetDto {
        ProviderEndpointPresetDto {
            id: label.to_owned(),
            label: label.to_owned(),
            base_url: base_url.map(str::to_owned),
            is_default,
        }
    }

    fn provider(endpoints: Vec<ProviderEndpointPresetDto>, env: &[&str]) -> ProviderSpecDto {
        ProviderSpecDto {
            id: "anthropic".to_owned(),
            display_name: "Anthropic".to_owned(),
            provider_kind: "anthropic".to_owned(),
            wire_format: ProviderWireFormatDto::AnthropicMessages,
            auth_scheme: ProviderAuthSchemeDto::XApiKey,
            default_model: "claude-sonnet".to_owned(),
            api_key_env_vars: env.iter().map(|var| (*var).to_owned()).collect(),
            endpoints,
            capabilities: capabilities(false, false, false, false),
        }
    }

    fn profile(name: &str, base_url: &str, models: &[&str], has_api_key: bool) -> ProfileDto {
        ProfileDto {
            name: name.to_owned(),
            provider_kind: "anthropic".to_owned(),
            wire_format: ProviderWireFormatDto::AnthropicMessages,
            auth_scheme: ProviderAuthSchemeDto::XApiKey,
            base_url: base_url.to_owned(),
            has_api_key,
            models: models
                .iter()
                .map(|id| ModelDto {
                    id: (*id).to_owned(),
                    model_options: None,
                    thinking: None,
                    thinking_capability: None,
                })
                .collect(),
        }
    }

    #[test]
    fn a_blank_value_is_omitted_rather_than_sent_as_an_empty_string() {
        assert_eq!(optional("  "), None);
        assert_eq!(optional(" fast ").as_deref(), Some("fast"));
        // 只读行上的空值要有占位，否则那两行看起来像没渲染出来。
        assert_eq!(non_empty_or("", "-"), "-");
        assert_eq!(non_empty_or(" gpt-5 ", "-"), " gpt-5 ");
    }

    #[test]
    fn preset_rows_fall_back_in_the_same_order_as_the_ported_implementation() {
        let spec = provider(
            vec![endpoint("官方", Some("https://api.anthropic.com"), true)],
            &["ANTHROPIC_API_KEY"],
        );
        let configured = profile("anthropic", "", &["claude-sonnet"], false);

        // 没有 baseUrl 的 profile 不算命中，退回默认 endpoint。
        assert_eq!(
            displayed_base_url(Some(&configured), &spec),
            "https://api.anthropic.com"
        );
        assert_eq!(displayed_base_url(None, &spec), "https://api.anthropic.com");
        assert_eq!(
            preset_key_hint(Some(&configured), &spec),
            "Key env:ANTHROPIC_API_KEY"
        );

        let with_key = profile("anthropic", "https://proxy", &["claude-sonnet"], true);
        assert_eq!(displayed_base_url(Some(&with_key), &spec), "https://proxy");
        assert_eq!(preset_key_hint(Some(&with_key), &spec), "Key 已配置");

        // endpoint 只有标签时显示标签，两者都没有时是占位。
        let labeled = provider(vec![endpoint("中转", None, true)], &[]);
        assert_eq!(displayed_base_url(None, &labeled), "中转");
        assert_eq!(preset_key_hint(None, &labeled), "Key 未配置");
        let bare = provider(vec![], &[]);
        assert_eq!(displayed_base_url(None, &bare), "-");
    }

    #[test]
    fn configured_model_keeps_the_selection_when_it_still_exists() {
        let target = profile("p", "", &["a", "b"], false);
        assert_eq!(configured_model(&target, "b"), Some("b"));
        // 选区里的模型不在这个 profile 里就退回第一个，一个模型都没有时给 `None`。
        assert_eq!(configured_model(&target, "zzz"), Some("a"));
        let modelless = profile("p", "", &[], false);
        assert_eq!(configured_model(&modelless, "a"), None);
    }

    #[test]
    fn dialog_defaults_prefer_the_existing_profile_then_the_provider() {
        let spec = provider(
            vec![endpoint("官方", Some("https://api.anthropic.com"), true)],
            &[],
        );
        let existing = profile("anthropic", "https://proxy", &["claude-haiku"], false);
        assert_eq!(
            dialog_default_base_url(&spec, Some(&existing)),
            "https://proxy"
        );
        assert_eq!(
            dialog_default_base_url(&spec, None),
            "https://api.anthropic.com"
        );

        // 预填的模型按「选区命中 → profile 的第一个 → provider 默认」三级退。
        let multi = profile("anthropic", "", &["claude-haiku", "claude-sonnet"], false);
        assert_eq!(
            dialog_default_model(&spec, Some(&multi), "claude-sonnet"),
            "claude-sonnet"
        );
        assert_eq!(
            dialog_default_model(&spec, Some(&multi), "unknown"),
            "claude-haiku"
        );
        assert_eq!(
            dialog_default_model(&spec, None, "claude-sonnet"),
            "claude-sonnet"
        );
    }

    #[test]
    fn capability_labels_follow_the_declared_support() {
        let mut spec = provider(vec![], &[]);
        spec.capabilities = capabilities(true, false, true, false);
        assert_eq!(capability_labels(&spec), ["Cache key", "Reasoning"]);
        spec.capabilities = capabilities(false, false, false, false);
        assert!(capability_labels(&spec).is_empty());
    }

    #[test]
    fn thinking_choices_hide_the_disable_option_when_the_model_refuses_it() {
        let locked = ThinkingCapabilityDto {
            allowed_effort: None,
            budget_min: None,
            budget_max: None,
            can_disable: false,
        };
        let choices = thinking_mode_choices(&locked);
        let values: Vec<&str> = choices.iter().map(|choice| choice.value.as_str()).collect();
        assert_eq!(values, ["default", "enabled"]);
        assert_eq!(mode_from_value("disabled"), ThinkingFormMode::Disabled);
        assert_eq!(mode_from_value("nonsense"), ThinkingFormMode::Default);
        assert_eq!(mode_value(ThinkingFormMode::Enabled), "enabled");

        // 能力没声明可选努力层级时只有「请选择」这一项。
        assert_eq!(effort_choices(&locked).len(), 1);
    }
}

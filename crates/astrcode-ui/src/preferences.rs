//! 界面偏好：界面对偏好文件的那一半。
//!
//! 偏好本身存在服务端（`~/.astrcode/ui-preferences.json`，见 `astrcode-storage` 的
//! `ui_preferences`）。这里放三件事：偏好在 UI 侧的当前值（写回是整份替换，要有个出处）、
//! 侧边栏宽度的可用区间，以及旧 localStorage 键到偏好载荷的一次性迁移。
//! 读 localStorage 是宿主的职责，只有 Web 宿主有它，所以这里只接值。

use astrcode_protocol::http::{UiPreferencesResponseDto, UpdateUiPreferencesRequest};

use crate::kanban::{forget_project_path, remember_project_path};

/// 侧边栏宽度的默认值（px），与旧前端的 `useSidebarResize` 一致。
pub const SIDEBAR_WIDTH_DEFAULT: f64 = 300.0;
/// 侧边栏宽度的下限（px）。
pub const SIDEBAR_WIDTH_MIN: f64 = 240.0;
/// 侧边栏宽度的上限（px）。
pub const SIDEBAR_WIDTH_MAX: f64 = 380.0;

/// 宽度是否落在可用区间内。
///
/// 宽度跨 localStorage、偏好文件和 HTTP 三条边界，读到的时候要重新判一次：写它的那一端
/// 可能是旧版本，文件也可能被手改过。区间外的宽度在界面上没有意义，按「没有这个值」处理。
pub fn is_usable_sidebar_width(width: f64) -> bool {
    width.is_finite() && (SIDEBAR_WIDTH_MIN..=SIDEBAR_WIDTH_MAX).contains(&width)
}

/// 旧前端留在 localStorage 里的 4 个键的原始值。
///
/// 迁移在这些键上一次性发生：读过就该丢掉，见 [`legacy_preferences_seed`]。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LegacyPreferenceStrings {
    /// `astrcode-sidebar-width`：裸数字字符串，不是 JSON。
    pub sidebar_width: Option<String>,
    /// `astrcode:collapsedProjectDirs`：JSON 字符串数组。
    pub collapsed_project_dirs: Option<String>,
    /// `astrcode:kanbanProjectPathHistory`：JSON 字符串数组。
    pub kanban_project_paths: Option<String>,
    /// `astrcode:kanbanIgnoredProjectPaths`：JSON 字符串数组。
    pub kanban_ignored_project_paths: Option<String>,
}

/// 界面偏好在 UI 侧的当前值。
///
/// `PUT /api/preferences` 是整份替换，所以写回时「此刻的其余字段」必须有个出处：本类型
/// 就是那个出处。宽度与折叠集合由外壳改写，看板那两项归看板页，这里一概原样带着。
pub struct UiPreferences {
    pub sidebar_width: f64,
    pub collapsed_project_dirs: Vec<String>,
    pub kanban_project_paths: Vec<String>,
    pub kanban_ignored_project_paths: Vec<String>,
}

impl UiPreferences {
    /// 取服务端响应里的偏好。
    ///
    /// 宽度跨了 HTTP 与偏好文件两道边界，用之前重判一次：区间外或非有限值当成「没有这个
    /// 值」，退回默认宽度。响应里的折叠集合与看板路径直接采信——它们的合法性由各自的消费
    /// 者决定（不存在的目录在读列表时被剪掉）。
    pub fn from_response(response: &UiPreferencesResponseDto) -> Self {
        let sidebar_width = if is_usable_sidebar_width(response.sidebar_width) {
            response.sidebar_width
        } else {
            SIDEBAR_WIDTH_DEFAULT
        };
        Self {
            sidebar_width,
            collapsed_project_dirs: response.collapsed_project_dirs.clone(),
            kanban_project_paths: response.kanban_project_paths.clone(),
            kanban_ignored_project_paths: response.kanban_ignored_project_paths.clone(),
        }
    }

    /// 界面还没有偏好时的初值。
    pub fn defaults() -> Self {
        Self {
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            collapsed_project_dirs: Vec::new(),
            kanban_project_paths: Vec::new(),
            kanban_ignored_project_paths: Vec::new(),
        }
    }

    /// 落盘用的请求体：整份替换，所以这里带上此刻的全部字段。
    pub fn update_request(&self) -> UpdateUiPreferencesRequest {
        UpdateUiPreferencesRequest {
            sidebar_width: self.sidebar_width,
            collapsed_project_dirs: self.collapsed_project_dirs.clone(),
            kanban_project_paths: self.kanban_project_paths.clone(),
            kanban_ignored_project_paths: self.kanban_ignored_project_paths.clone(),
        }
    }

    /// 宽度在写回前夹进可用区间，并返回夹取后的值；返回值同时用于界面当前宽度。
    pub fn set_sidebar_width(&mut self, width: f64) -> f64 {
        let width = if width.is_finite() {
            width.clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX)
        } else {
            SIDEBAR_WIDTH_DEFAULT
        };
        self.sidebar_width = width;
        width
    }

    /// 直接采用来自服务端的偏好（迁移种子落盘后的回读）。
    pub fn adopt(&mut self, response: &UiPreferencesResponseDto) {
        *self = Self::from_response(response);
    }
}

/// 偏好取回之前发生的一次看板路径改动。
///
/// 存操作而不是结果：记住与忘掉是有序的，先记住再忘掉与反过来不是一回事。
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathEdit {
    Remember(String),
    Forget(String),
}

/// 偏好取回之前攒下的本地改动。
///
/// 那段时间本地那份还不是「值」而是 delta：基准（服务端存着的那份）还没到，照着本地那份做
/// 整份替换，会把没动过的字段一并打回初值（宽度回落、看板路径被清空）。所以改动先记在这里，
/// 等基准到了按字段重放——见 [`PendingPreferences::merge`]。
#[derive(Debug, Default)]
pub struct PendingPreferences {
    /// 最后一次拖拽后的宽度。
    sidebar_width: Option<f64>,
    /// 最后一次的折叠集合。
    collapsed_project_dirs: Option<Vec<String>>,
    /// 看板路径上的改动，按发生顺序重放。
    path_edits: Vec<PathEdit>,
}

impl PendingPreferences {
    pub fn set_sidebar_width(&mut self, width: f64) {
        self.sidebar_width = Some(width);
    }

    pub fn set_collapsed_project_dirs(&mut self, collapsed: Vec<String>) {
        self.collapsed_project_dirs = Some(collapsed);
    }

    pub fn remember_project_path(&mut self, working_dir: &str) {
        self.path_edits
            .push(PathEdit::Remember(working_dir.to_string()));
    }

    pub fn forget_project_path(&mut self, working_dir: &str) {
        self.path_edits
            .push(PathEdit::Forget(working_dir.to_string()));
    }

    /// 空日志表示「没有改动待补写」，取回那一步据此决定要不要再写一次。
    pub fn is_empty(&self) -> bool {
        self.sidebar_width.is_none()
            && self.collapsed_project_dirs.is_none()
            && self.path_edits.is_empty()
    }

    /// 把攒下的改动重放到刚取回的基准上，得到此刻应有的偏好。
    pub fn merge(self, mut base: UiPreferences) -> UiPreferences {
        if let Some(width) = self.sidebar_width {
            base.sidebar_width = width;
        }
        if let Some(collapsed) = self.collapsed_project_dirs {
            base.collapsed_project_dirs = collapsed;
        }
        for edit in self.path_edits {
            let (paths, ignored) = match &edit {
                PathEdit::Remember(working_dir) => remember_project_path(
                    &base.kanban_project_paths,
                    &base.kanban_ignored_project_paths,
                    working_dir,
                ),
                PathEdit::Forget(working_dir) => forget_project_path(
                    &base.kanban_project_paths,
                    &base.kanban_ignored_project_paths,
                    working_dir,
                ),
            };
            base.kanban_project_paths = paths;
            base.kanban_ignored_project_paths = ignored;
        }
        base
    }
}

/// 把旧键转换成一次性的迁移载荷。
///
/// `stored` 为真表示服务端已经有偏好文件，此时一律返回 `None`：迁移只能发生一次，
/// 服务端一旦有内容就不能再被旧键覆盖，否则用户清空偏好之后旧值会自己回来。
///
/// 返回 `None` 还有另一层意思——这些键里没有可迁移的内容，此时不要写入，保持服务端为空，
/// 让别的浏览器还有机会迁移。
pub fn legacy_preferences_seed(
    stored: bool,
    legacy: &LegacyPreferenceStrings,
) -> Option<UpdateUiPreferencesRequest> {
    if stored {
        return None;
    }

    let sidebar_width = legacy.sidebar_width.as_deref().and_then(parse_legacy_width);
    let collapsed_project_dirs = parse_optional_list(legacy.collapsed_project_dirs.as_deref());
    let kanban_project_paths = parse_optional_list(legacy.kanban_project_paths.as_deref());
    let kanban_ignored_project_paths =
        parse_optional_list(legacy.kanban_ignored_project_paths.as_deref());

    if sidebar_width.is_none()
        && collapsed_project_dirs.is_empty()
        && kanban_project_paths.is_empty()
        && kanban_ignored_project_paths.is_empty()
    {
        return None;
    }

    Some(UpdateUiPreferencesRequest {
        sidebar_width: sidebar_width.unwrap_or(SIDEBAR_WIDTH_DEFAULT),
        collapsed_project_dirs,
        kanban_project_paths,
        kanban_ignored_project_paths,
    })
}

/// 旧键是裸数字字符串；区间外的值按「没有这个键」处理。
fn parse_legacy_width(raw: &str) -> Option<f64> {
    let width: f64 = raw.trim().parse().ok()?;
    is_usable_sidebar_width(width).then_some(width)
}

/// 旧键是 JSON 字符串数组；与旧前端的 `readStringList` 一致，有一项不是字符串就整份丢弃。
fn parse_optional_list(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let Ok(items) = serde_json::from_str::<Vec<String>>(raw) else {
        return Vec::new();
    };
    items
        .into_iter()
        .map(|dir| dir.trim().to_string())
        .filter(|dir| !dir.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width(value: &str) -> Option<String> {
        Some(value.to_string())
    }

    fn list(value: &str) -> Option<String> {
        Some(value.to_string())
    }

    #[test]
    fn a_stored_server_never_gets_overwritten_by_legacy_keys() {
        let legacy = LegacyPreferenceStrings {
            sidebar_width: width("264"),
            collapsed_project_dirs: list(r#"["/w/alpha"]"#),
            ..Default::default()
        };

        assert!(legacy_preferences_seed(true, &legacy).is_none());
    }

    #[test]
    fn an_empty_server_takes_the_whole_legacy_payload() {
        let legacy = LegacyPreferenceStrings {
            sidebar_width: width("264"),
            collapsed_project_dirs: list(r#"["/w/alpha"]"#),
            kanban_project_paths: list(r#"["/w/beta","/w/gamma"]"#),
            kanban_ignored_project_paths: list(r#"["/w/delta"]"#),
        };

        let seed = legacy_preferences_seed(false, &legacy).expect("有可迁移的内容");

        assert_eq!(seed.sidebar_width, 264.0);
        assert_eq!(seed.collapsed_project_dirs, vec!["/w/alpha".to_string()]);
        assert_eq!(
            seed.kanban_project_paths,
            vec!["/w/beta".to_string(), "/w/gamma".to_string()]
        );
        assert_eq!(
            seed.kanban_ignored_project_paths,
            vec!["/w/delta".to_string()]
        );
    }

    #[test]
    fn nothing_usable_is_not_a_seed_so_the_server_stays_open_for_other_browsers() {
        let legacy = LegacyPreferenceStrings {
            sidebar_width: width("not a number"),
            collapsed_project_dirs: list("{}"),
            kanban_project_paths: list(""),
            kanban_ignored_project_paths: None,
        };

        assert!(legacy_preferences_seed(false, &legacy).is_none());
    }

    #[test]
    fn an_out_of_range_width_falls_back_to_the_default_and_keeps_the_lists() {
        let legacy = LegacyPreferenceStrings {
            sidebar_width: width("9000"),
            kanban_project_paths: list(r#"["/w/beta"]"#),
            ..Default::default()
        };

        let seed = legacy_preferences_seed(false, &legacy).expect("列表仍可迁移");

        assert_eq!(seed.sidebar_width, SIDEBAR_WIDTH_DEFAULT);
        assert_eq!(seed.kanban_project_paths, vec!["/w/beta".to_string()]);
    }

    #[test]
    fn a_list_with_a_non_string_item_is_dropped_whole() {
        let legacy = LegacyPreferenceStrings {
            kanban_project_paths: list(r#"["/w/beta",7]"#),
            collapsed_project_dirs: list(r#"["  ", "/w/alpha  "]"#),
            ..Default::default()
        };

        let seed = legacy_preferences_seed(false, &legacy).expect("折叠列表仍可迁移");

        assert!(seed.kanban_project_paths.is_empty());
        assert_eq!(seed.collapsed_project_dirs, vec!["/w/alpha".to_string()]);
    }

    #[test]
    fn width_bounds_are_inclusive_and_reject_non_finite_values() {
        assert!(is_usable_sidebar_width(SIDEBAR_WIDTH_MIN));
        assert!(is_usable_sidebar_width(SIDEBAR_WIDTH_MAX));
        assert!(is_usable_sidebar_width(SIDEBAR_WIDTH_DEFAULT));
        assert!(!is_usable_sidebar_width(SIDEBAR_WIDTH_MIN - 1.0));
        assert!(!is_usable_sidebar_width(SIDEBAR_WIDTH_MAX + 1.0));
        assert!(!is_usable_sidebar_width(f64::NAN));
        assert!(!is_usable_sidebar_width(f64::INFINITY));
    }

    #[test]
    fn a_pending_edit_replays_onto_the_stored_base() {
        let base = UiPreferences {
            sidebar_width: 264.0,
            collapsed_project_dirs: vec!["/w/alpha".to_owned()],
            kanban_project_paths: vec!["/w/stored".to_owned()],
            kanban_ignored_project_paths: vec!["/w/hidden".to_owned()],
        };
        let mut pending = PendingPreferences::default();
        pending.set_collapsed_project_dirs(vec!["/w/beta".to_owned()]);
        pending.remember_project_path("/w/new");

        let merged = pending.merge(base);

        // 动过的字段取那次改动，没动过的字段仍是服务端存着的那份。
        assert_eq!(merged.collapsed_project_dirs, vec!["/w/beta".to_owned()]);
        assert_eq!(merged.sidebar_width, 264.0);
        assert_eq!(
            merged.kanban_project_paths,
            vec!["/w/new".to_owned(), "/w/stored".to_owned()]
        );
        assert_eq!(
            merged.kanban_ignored_project_paths,
            vec!["/w/hidden".to_owned()]
        );
    }

    #[test]
    fn two_edits_on_the_same_path_are_replayed_in_order() {
        let mut pending = PendingPreferences::default();
        pending.remember_project_path("/w/a");
        pending.forget_project_path("/w/a");

        let merged = pending.merge(UiPreferences::defaults());

        assert!(merged.kanban_project_paths.is_empty());
        assert_eq!(merged.kanban_ignored_project_paths, vec!["/w/a".to_owned()]);
    }

    #[test]
    fn an_empty_journal_means_there_is_nothing_to_write_back() {
        assert!(PendingPreferences::default().is_empty());

        let mut pending = PendingPreferences::default();
        pending.set_sidebar_width(320.0);

        assert!(!pending.is_empty());
        assert_eq!(
            pending.merge(UiPreferences::defaults()).sidebar_width,
            320.0
        );
    }

    fn response(width: f64) -> UiPreferencesResponseDto {
        UiPreferencesResponseDto {
            stored: true,
            sidebar_width: width,
            collapsed_project_dirs: vec!["/w/alpha".to_owned()],
            kanban_project_paths: vec!["/w/beta".to_owned()],
            kanban_ignored_project_paths: vec!["/w/gamma".to_owned()],
        }
    }

    #[test]
    fn a_stored_width_outside_the_range_falls_back_to_the_default() {
        assert_eq!(
            UiPreferences::from_response(&response(9000.0)).sidebar_width,
            SIDEBAR_WIDTH_DEFAULT
        );
        assert_eq!(
            UiPreferences::from_response(&response(264.0)).sidebar_width,
            264.0
        );
    }

    #[test]
    fn the_update_request_carries_the_fields_it_does_not_own() {
        let mut preferences = UiPreferences::from_response(&response(264.0));
        preferences.set_sidebar_width(300.0);
        let request = preferences.update_request();

        assert_eq!(request.sidebar_width, 300.0);
        assert_eq!(request.collapsed_project_dirs, vec!["/w/alpha".to_owned()]);
        assert_eq!(request.kanban_project_paths, vec!["/w/beta".to_owned()]);
        assert_eq!(
            request.kanban_ignored_project_paths,
            vec!["/w/gamma".to_owned()]
        );
    }

    #[test]
    fn dragging_beyond_the_range_is_clamped_at_both_ends() {
        let mut preferences = UiPreferences::defaults();
        assert_eq!(preferences.set_sidebar_width(10.0), SIDEBAR_WIDTH_MIN);
        assert_eq!(preferences.set_sidebar_width(9999.0), SIDEBAR_WIDTH_MAX);
        assert_eq!(
            preferences.set_sidebar_width(f64::NAN),
            SIDEBAR_WIDTH_DEFAULT
        );
        // 夹取后的值同时就是写回的宽度。
        assert_eq!(preferences.sidebar_width, SIDEBAR_WIDTH_DEFAULT);
    }
}

//! UI 偏好落盘：`ui-preferences.json`，默认与 `config.toml` 同目录。
//!
//! 这里只按形状存取，不做区间钳制与列表裁剪：宽度区间是界面布局的事，候选上限是
//! 候选项策略的事，都在界面侧（`astrcode-ui`）。服务端存原值、读原值。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    StorageError,
    durable_write::{replace_durable_file, spawn_blocking_storage},
};

/// 偏好文件名；与 `config.toml` 同目录。
const UI_PREFERENCES_FILE_NAME: &str = "ui-preferences.json";

/// 侧边栏宽度的默认值（px），与 `astrcode-ui` 的布局默认值一致。
const SIDEBAR_WIDTH_DEFAULT: f64 = 300.0;

/// 跨会话保留的界面状态。
///
/// 字段与旧前端的 4 个 localStorage 键一一对应。旧键只迁移一次，闸门见
/// `astrcode-ui::preferences::legacy_preferences_seed`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiPreferences {
    /// 侧边栏宽度（px）。
    pub sidebar_width: f64,
    /// 被折叠起来的项目分组。
    pub collapsed_project_dirs: Vec<String>,
    /// 新建卡片用过的项目路径，最近使用在前。
    pub kanban_project_paths: Vec<String>,
    /// 被用户从候选里删掉的路径。
    pub kanban_ignored_project_paths: Vec<String>,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            collapsed_project_dirs: Vec::new(),
            kanban_project_paths: Vec::new(),
            kanban_ignored_project_paths: Vec::new(),
        }
    }
}

/// 一次读取的结果。
///
/// `stored` 记录「这台机器上有没有偏好文件」，与内容分开：文件存在但读不出来时
/// 不能算「没有偏好」，否则旧 localStorage 会反复把过期值灌回来。
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedUiPreferences {
    pub preferences: UiPreferences,
    pub stored: bool,
}

/// 文件系统实现，复用与 `config.toml` 相同的原子写语义。
pub struct FileUiPreferencesStore {
    path: PathBuf,
}

impl FileUiPreferencesStore {
    /// 偏好文件放在配置文件旁边。
    ///
    /// 测试和嵌入式启动用自定义的 `config.toml` 路径做隔离，偏好文件必须跟着走，
    /// 否则会写进真实的 `~/.astrcode/`。
    pub fn alongside_config(config_path: &Path) -> Self {
        Self::new(sibling_path(config_path, UI_PREFERENCES_FILE_NAME))
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取偏好。文件不存在或无法解析时都退回默认值，由 [`LoadedUiPreferences::stored`]
    /// 区分；下一次写入会覆盖坏文件。
    pub async fn load(&self) -> Result<LoadedUiPreferences, StorageError> {
        let path = self.path.clone();
        spawn_blocking_storage("ui preferences load", move || {
            if !path.exists() {
                return Ok(LoadedUiPreferences {
                    preferences: UiPreferences::default(),
                    stored: false,
                });
            }
            let preferences = match read_preferences(&path) {
                Ok(preferences) => preferences,
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        "读取 UI 偏好失败，退回默认值: {error}"
                    );
                    UiPreferences::default()
                },
            };
            Ok(LoadedUiPreferences {
                preferences,
                stored: true,
            })
        })
        .await
    }

    pub async fn save(&self, preferences: &UiPreferences) -> Result<(), StorageError> {
        let path = self.path.clone();
        let bytes = serde_json::to_vec_pretty(preferences)?;
        spawn_blocking_storage("ui preferences save", move || {
            replace_durable_file(&path, &bytes).map_err(StorageError::from)
        })
        .await
    }
}

fn read_preferences(path: &Path) -> Result<UiPreferences, StorageError> {
    let data = std::fs::read_to_string(path)?;
    serde_json::from_str(&data).map_err(StorageError::from)
}

fn sibling_path(path: &Path, file_name: &str) -> PathBuf {
    match path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => parent.join(file_name),
        None => PathBuf::from(file_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> UiPreferences {
        UiPreferences {
            sidebar_width: 264.0,
            collapsed_project_dirs: vec!["/home/me/alpha".to_string()],
            kanban_project_paths: vec!["/home/me/beta".to_string()],
            kanban_ignored_project_paths: vec!["/home/me/gamma".to_string()],
        }
    }

    #[tokio::test]
    async fn save_then_load_round_trips_every_field() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileUiPreferencesStore::new(directory.path().join(UI_PREFERENCES_FILE_NAME));

        store.save(&sample()).await.unwrap();
        let loaded = store.load().await.unwrap();

        assert_eq!(loaded.preferences, sample());
        assert!(loaded.stored);
    }

    #[tokio::test]
    async fn missing_file_reports_defaults_and_not_stored() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileUiPreferencesStore::new(directory.path().join(UI_PREFERENCES_FILE_NAME));

        let loaded = store.load().await.unwrap();

        assert_eq!(loaded.preferences, UiPreferences::default());
        assert!(!loaded.stored);
    }

    #[tokio::test]
    async fn unparsable_file_keeps_the_stored_flag_so_migration_stays_spent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(UI_PREFERENCES_FILE_NAME);
        std::fs::write(&path, "{ this is not json").unwrap();
        let store = FileUiPreferencesStore::new(path);

        let loaded = store.load().await.unwrap();

        assert_eq!(loaded.preferences, UiPreferences::default());
        assert!(loaded.stored);
    }

    #[tokio::test]
    async fn saving_leaves_no_temporary_files_behind() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileUiPreferencesStore::new(directory.path().join(UI_PREFERENCES_FILE_NAME));

        store.save(&sample()).await.unwrap();

        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn alongside_config_puts_the_file_next_to_the_config() {
        let store =
            FileUiPreferencesStore::alongside_config(Path::new("/home/me/.astrcode/config.toml"));

        assert_eq!(
            store.path(),
            Path::new("/home/me/.astrcode/ui-preferences.json")
        );
    }

    #[test]
    fn alongside_config_handles_a_bare_file_name() {
        let store = FileUiPreferencesStore::alongside_config(Path::new("config.toml"));

        assert_eq!(store.path(), Path::new(UI_PREFERENCES_FILE_NAME));
    }
}

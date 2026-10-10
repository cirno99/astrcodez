//! 文件树状态：根下一层与已展开目录的各一层条目，都按展开动作懒取。
//!
//! 这里只有状态与推导，渲染在 [`super`]；`visible_rows` 是纯函数，可以脱离窗口测试。
//!
//! 折叠会丢掉该目录取到的内容：再展开时重新取一遍。这既让「取失败」有自然的重试路径，
//! 也让 agent 刚建出的文件在折叠再展开后就会出现，不必额外做失效判断。

use astrcode_protocol::http::FileEntryDto;
use rustc_hash::FxHashMap;

/// 一个目录取到的一层内容；取失败时 `entries` 为空、`error` 有值。
#[derive(Default)]
struct DirContent {
    entries: Vec<FileEntryDto>,
    error: Option<String>,
}

/// 树的展开状态与已取到的目录内容。
#[derive(Default)]
pub(super) struct FileTree {
    /// 已取过的目录，键是相对浏览根目录的路径（根目录是空串）。
    dirs: FxHashMap<String, DirContent>,
    /// 展开着的目录。
    expanded: Vec<String>,
    /// 请求在飞的目录。
    loading: Vec<String>,
}

/// 树里要显示的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TreeRow {
    /// 相对浏览根目录的路径。
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    /// 缩进层级：根下一层是 0。
    pub depth: usize,
    /// 目录是否展开。
    pub expanded: bool,
    /// 这一层的内容是否还在取。
    pub loading: bool,
    /// 展开着但目前没有内容：取失败，或者空目录。
    pub error: Option<String>,
}

impl FileTree {
    /// 标记一个目录的内容正在取。
    pub(super) fn begin_loading(&mut self, path: &str) {
        if !self.loading.iter().any(|loading| loading == path) {
            self.loading.push(path.to_owned());
        }
    }

    /// 记下一个目录取到的内容。
    pub(super) fn set_dir(&mut self, path: &str, entries: Vec<FileEntryDto>) {
        self.loading.retain(|loading| loading != path);
        self.dirs.insert(
            path.to_owned(),
            DirContent {
                entries,
                error: None,
            },
        );
    }

    /// 记下一个目录取失败。
    pub(super) fn set_error(&mut self, path: &str, message: String) {
        self.loading.retain(|loading| loading != path);
        self.dirs.insert(
            path.to_owned(),
            DirContent {
                entries: Vec::new(),
                error: Some(message),
            },
        );
    }

    /// 这个目录的内容是否已经取过（取失败也算取过，界面会就地显示原因）。
    pub(super) fn is_loaded(&self, path: &str) -> bool {
        self.dirs.contains_key(path)
    }

    pub(super) fn is_expanded(&self, path: &str) -> bool {
        self.expanded.iter().any(|expanded| expanded == path)
    }

    /// 切换一个目录的展开状态，返回切换后的状态。
    pub(super) fn toggle(&mut self, path: &str) -> bool {
        if self.is_expanded(path) {
            self.expanded.retain(|expanded| expanded != path);
            // 折叠即失效：见模块头注释。
            self.dirs.remove(path);
            self.loading.retain(|loading| loading != path);
            return false;
        }
        self.expanded.push(path.to_owned());
        true
    }

    /// 丢掉全部缓存，回到「只有根、什么都没取」的状态。
    pub(super) fn clear(&mut self) {
        self.dirs.clear();
        self.loading.clear();
        self.expanded.clear();
    }

    /// 当前应显示的行：从根开始，只走进展开着的目录。
    pub(super) fn visible_rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        self.push_rows("", 0, &mut rows);
        rows
    }

    fn push_rows(&self, path: &str, depth: usize, rows: &mut Vec<TreeRow>) {
        let Some(dir) = self.dirs.get(path) else {
            return;
        };
        let loading = self.loading.iter().any(|loading| loading == path);
        let expanded = self.is_expanded(path);
        for entry in &dir.entries {
            let entry_expanded = entry.is_dir && self.is_expanded(&entry.path);
            rows.push(TreeRow {
                path: entry.path.clone(),
                name: entry.name.clone(),
                is_dir: entry.is_dir,
                depth,
                expanded: entry_expanded,
                loading: entry_expanded && !self.is_loaded(&entry.path),
                error: None,
            });
            if entry_expanded {
                self.push_rows(&entry.path, depth + 1, rows);
            }
        }
        // 展开着的目录取不到内容时，把原因挂在它自己下面那一层的位置上。
        if expanded && let Some(error) = &dir.error {
            rows.push(TreeRow {
                path: path.to_owned(),
                name: error.clone(),
                is_dir: false,
                depth,
                expanded: false,
                loading,
                error: Some(error.clone()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, path: &str, is_dir: bool) -> FileEntryDto {
        FileEntryDto {
            name: name.to_owned(),
            path: path.to_owned(),
            is_dir,
        }
    }

    /// 一棵两层树：根下有 src/ 与 README.md，src/ 下有 main.rs。
    fn tree() -> FileTree {
        let mut tree = FileTree::default();
        tree.set_dir(
            "",
            vec![
                entry("src", "src", true),
                entry("README.md", "README.md", false),
            ],
        );
        tree.set_dir("src", vec![entry("main.rs", "src/main.rs", false)]);
        tree
    }

    #[test]
    fn collapsed_directories_hide_their_children() {
        let tree = tree();
        let rows = tree.visible_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].path, "src");
        assert!(!rows[0].expanded);
        assert_eq!(rows[1].path, "README.md");
    }

    #[test]
    fn expanding_reveals_children_with_depth() {
        let mut tree = tree();
        assert!(tree.toggle("src"));
        let rows = tree.visible_rows();
        assert_eq!(
            rows.iter()
                .map(|row| (row.path.as_str(), row.depth))
                .collect::<Vec<_>>(),
            [("src", 0), ("src/main.rs", 1), ("README.md", 0)]
        );
    }

    #[test]
    fn collapsing_drops_the_cached_directory() {
        let mut tree = tree();
        tree.toggle("src");
        assert!(!tree.toggle("src"));
        assert!(!tree.is_loaded("src"));
        assert!(
            !tree
                .visible_rows()
                .iter()
                .any(|row| row.path == "src/main.rs")
        );
    }

    #[test]
    fn load_failure_is_shown_at_the_expanded_directory() {
        let mut tree = tree();
        tree.toggle("src");
        tree.set_error("src", "权限不足".to_owned());
        let rows = tree.visible_rows();
        // src 自己的行还在，失败原因占它下面那一层的位置。
        assert_eq!(rows[1].name, "权限不足");
        assert_eq!(rows[1].error.as_deref(), Some("权限不足"));
        assert_eq!(rows[1].depth, 1);
    }

    #[test]
    fn expanded_but_not_yet_loaded_reports_loading() {
        let mut tree = tree();
        tree.toggle("src");
        tree.clear();
        tree.set_dir("", vec![entry("src", "src", true)]);
        tree.toggle("src");
        let rows = tree.visible_rows();
        assert!(rows[0].loading);
    }
}

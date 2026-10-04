//! 看板（`astrcode-kanban` 扩展）的宿主侧词汇与逻辑。
//!
//! 线缆形状归扩展所有（[`wire`]），日历分桶（[`calendar`]）与候选列表（[`path_history`]）
//! 是宿主的职责。桌面 App 与 Web UI 共用这一层，视图只负责画。
//!
//! 页面把六列拆成两个区域：公共卡片区（四格）与日历区（每个时间桶两个手风琴项）。
//! 两个区域合起来覆盖全部六列，不新增也不隐藏任何一列。

use astrcode_protocol::http::ExtensionStateDto;

pub mod calendar;
pub mod path_history;
pub mod wire;

pub use calendar::{
    CalendarBucket, CalendarScale, UNSCHEDULED_BUCKET_KEY, anchor_label, bucket_key_of,
    buckets_for, card_day_key, day_key_from_iso, day_key_of, day_key_to_date, shift_anchor_day_key,
    today_key, unscheduled_cards,
};
pub use path_history::{
    IGNORED_PROJECT_PATH_LIMIT, PROJECT_PATH_HISTORY_LIMIT, forget_project_path,
    merge_project_path_candidates, normalize_path_list, remember_project_path,
};
pub use wire::{
    BoardResponse, Card, CardColumn, CreateCardRequest, DirectoryEntry, DirectoryListing,
    EXTENSION_ID, UpdateCardRequest,
};

/// 公共卡片区的四格，按需求从上到下等分。
pub const PUBLIC_AREA_COLUMNS: [CardColumn; 4] = [
    CardColumn::Ready,
    CardColumn::Analyzing,
    CardColumn::Implementing,
    CardColumn::Blocked,
];

/// 公共区里接受拖拽落点的格。
///
/// 运行中的两格是只读展示：卡片正被扩展持有，写进去只会拿到 400。
pub const PUBLIC_AREA_DROP_COLUMNS: [CardColumn; 2] = [CardColumn::Ready, CardColumn::Blocked];

/// 日历列里的手风琴菜单项，恰好对应 `backlog` / `done` 两列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalendarSlot {
    Backlog,
    Done,
}

impl CalendarSlot {
    pub const ALL: [Self; 2] = [Self::Backlog, Self::Done];

    pub fn column(self) -> CardColumn {
        match self {
            Self::Backlog => CardColumn::Backlog,
            Self::Done => CardColumn::Done,
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Backlog => Self::Done,
            Self::Done => Self::Backlog,
        }
    }
}

/// 一列里两个手风琴项各自的卡片数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlotCounts {
    pub backlog: usize,
    pub done: usize,
}

impl SlotCounts {
    pub fn get(self, slot: CalendarSlot) -> usize {
        match slot {
            CalendarSlot::Backlog => self.backlog,
            CalendarSlot::Done => self.done,
        }
    }
}

/// 一列里实际展开的那一项。
///
/// `None` 表示对半态：两项同时展开、各占一半高度，这是默认状态。
/// 用户点开某一项后进入手风琴态：优先用他点的那一项；它空而另一项有卡片时让位——
/// 否则卡片落进「已完成」后会被藏在一个收起的菜单项里，看起来像没落进去。
pub fn resolve_expanded_slot(
    preferred: Option<CalendarSlot>,
    counts: SlotCounts,
) -> Option<CalendarSlot> {
    let preferred = preferred?;
    if counts.get(preferred) > 0 {
        return Some(preferred);
    }
    let other = preferred.other();
    if counts.get(other) > 0 {
        Some(other)
    } else {
        Some(preferred)
    }
}

/// 用户可写入的列：运行中的两列由扩展独占。
pub fn is_user_writable(column: CardColumn) -> bool {
    !column.is_running()
}

/// 点击卡片会跳到对应对话的列。
///
/// 「待办」还没有对话，「待领取」按需求也不在跳转范围内——即使续跑退回的卡片仍留着
/// `sessionId`，点击也保持无反应。
pub fn opens_conversation(column: CardColumn) -> bool {
    matches!(
        column,
        CardColumn::Analyzing | CardColumn::Implementing | CardColumn::Done | CardColumn::Blocked
    )
}

/// 只有待办列的卡片允许改标题与正文；其余列要么正被扩展持有，要么已经产出了交付记录。
pub fn is_editable_column(column: CardColumn) -> bool {
    column == CardColumn::Backlog
}

/// 拖拽落点：公共区的某一格，或日历某个时间桶的某个手风琴项。
///
/// 结构相等即「同一落点」，因此不需要额外的比较函数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropTarget {
    Column(CardColumn),
    Bucket {
        bucket_key: String,
        slot: CalendarSlot,
    },
}

impl DropTarget {
    /// 落点对应的列；落在日历项上时由手风琴项决定。
    pub fn column(&self) -> CardColumn {
        match self {
            Self::Column(column) => *column,
            Self::Bucket { slot, .. } => slot.column(),
        }
    }
}

/// 卡片改动：拖拽同时改列与归属日，卡片上的下拉框只改列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardMove {
    pub column: CardColumn,
    pub date: Option<String>,
}

/// 看板入口是否可用：扩展在册、已启用、且已加载。
///
/// 三个条件缺一不可。装了但被禁用、或启用但加载失败时，页面没有可用路由可打，
/// 此时侧边栏不该给出入口（与 Web 前端的 `kanbanExtensionAvailable` 同判据）。
pub fn extension_available(extensions: &[ExtensionStateDto]) -> bool {
    extensions.iter().any(|extension| {
        extension.extension_id == EXTENSION_ID && extension.enabled && extension.loaded
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_areas_cover_every_column_exactly_once() {
        let mut covered: Vec<CardColumn> = PUBLIC_AREA_COLUMNS.to_vec();
        covered.extend(CalendarSlot::ALL.map(CalendarSlot::column));
        covered.sort_by_key(|column| CardColumn::ALL.iter().position(|c| c == column));

        assert_eq!(covered, CardColumn::ALL.to_vec());
    }

    #[test]
    fn running_columns_are_shown_but_never_accept_drops_or_writes() {
        for column in PUBLIC_AREA_COLUMNS {
            assert_eq!(
                PUBLIC_AREA_DROP_COLUMNS.contains(&column),
                is_user_writable(column),
                "{column:?}"
            );
        }
        assert_eq!(
            PUBLIC_AREA_DROP_COLUMNS,
            [CardColumn::Ready, CardColumn::Blocked]
        );
    }

    #[test]
    fn editable_and_conversation_columns_are_disjoint() {
        for column in CardColumn::ALL {
            assert!(
                !(is_editable_column(column) && opens_conversation(column)),
                "{column:?} 不能既可编辑又能跳转"
            );
        }
        assert!(is_editable_column(CardColumn::Backlog));
        assert!(!opens_conversation(CardColumn::Backlog));
        assert!(!opens_conversation(CardColumn::Ready));
        for column in [
            CardColumn::Analyzing,
            CardColumn::Implementing,
            CardColumn::Done,
            CardColumn::Blocked,
        ] {
            assert!(opens_conversation(column), "{column:?}");
        }
    }

    #[test]
    fn half_state_expands_both_slots() {
        let counts = SlotCounts {
            backlog: 2,
            done: 1,
        };
        assert_eq!(resolve_expanded_slot(None, counts), None);
    }

    #[test]
    fn the_accordion_keeps_the_clicked_slot_when_it_has_cards() {
        let counts = SlotCounts {
            backlog: 2,
            done: 1,
        };
        assert_eq!(
            resolve_expanded_slot(Some(CalendarSlot::Done), counts),
            Some(CalendarSlot::Done)
        );
    }

    /// 卡片落进「已完成」后不能藏在收起的「待办」项里。
    #[test]
    fn the_accordion_yields_to_the_slot_that_actually_has_cards() {
        let counts = SlotCounts {
            backlog: 0,
            done: 3,
        };
        assert_eq!(
            resolve_expanded_slot(Some(CalendarSlot::Backlog), counts),
            Some(CalendarSlot::Done)
        );
    }

    #[test]
    fn the_accordion_keeps_the_clicked_slot_when_both_are_empty() {
        assert_eq!(
            resolve_expanded_slot(Some(CalendarSlot::Backlog), SlotCounts::default()),
            Some(CalendarSlot::Backlog)
        );
    }

    #[test]
    fn drop_targets_compare_structurally() {
        assert_eq!(
            DropTarget::Column(CardColumn::Ready),
            DropTarget::Column(CardColumn::Ready)
        );
        assert_ne!(
            DropTarget::Column(CardColumn::Ready),
            DropTarget::Column(CardColumn::Blocked)
        );
        assert_ne!(
            DropTarget::Bucket {
                bucket_key: "2026-03-07".into(),
                slot: CalendarSlot::Backlog,
            },
            DropTarget::Bucket {
                bucket_key: "2026-03-08".into(),
                slot: CalendarSlot::Backlog,
            }
        );
    }

    #[test]
    fn a_bucket_drop_target_reports_the_slot_column() {
        let target = DropTarget::Bucket {
            bucket_key: "2026-03".into(),
            slot: CalendarSlot::Done,
        };
        assert_eq!(target.column(), CardColumn::Done);
        assert_eq!(
            DropTarget::Column(CardColumn::Blocked).column(),
            CardColumn::Blocked
        );
    }

    #[test]
    fn a_card_move_carries_the_column_and_an_optional_day() {
        let dropdown = CardMove {
            column: CardColumn::Blocked,
            date: None,
        };
        let drag = CardMove {
            column: CardColumn::Done,
            date: Some("2026-03-07".into()),
        };
        assert_ne!(dropdown, drag);
        assert_eq!(drag.date.as_deref(), Some("2026-03-07"));
    }

    #[test]
    fn the_entry_needs_the_extension_enabled_and_loaded() {
        fn extension(enabled: bool, loaded: bool) -> ExtensionStateDto {
            ExtensionStateDto {
                extension_id: EXTENSION_ID.to_string(),
                enabled,
                loaded,
                source: astrcode_protocol::wire::ExtensionSourceDto::Builtin,
                declaration: None,
                diagnostics: None,
            }
        }

        assert!(extension_available(&[extension(true, true)]));
        // 禁用或加载失败时页面没有可用路由可打，入口必须一起收起来。
        assert!(!extension_available(&[extension(false, true)]));
        assert!(!extension_available(&[extension(true, false)]));
        assert!(!extension_available(&[]));
    }
}

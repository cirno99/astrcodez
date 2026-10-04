//! 看板日历的纯日期逻辑：把卡片按归属日归进各刻度的时间桶。
//!
//! 只做日期运算，不碰 gpui 与网络，因此可以直接单测。
//! 所有日键都是补零的 `YYYY-MM-DD` 本地日期字符串——字符串比较即时间先后，
//! 也与后端 `Card.date` 的归一化格式一一对应；`bucket_key_of` 的月/年桶更是直接切片，
//! 这一致性由 [`day_key_to_date`] 的规范性校验兜住。

use chrono::{Datelike as _, Days, Local, Months, NaiveDate};

use super::wire::Card;

/// 看板日历支持的刻度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalendarScale {
    Day,
    Week,
    Month,
    Year,
}

impl CalendarScale {
    /// 四个刻度在页面上的固定顺序。
    pub const ALL: [Self; 4] = [Self::Day, Self::Week, Self::Month, Self::Year];

    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "日",
            Self::Week => "周",
            Self::Month => "月",
            Self::Year => "年",
        }
    }

    /// 该刻度是否直接在列里列出卡片。
    ///
    /// 年刻度只给数量：一列要塞进整年，列出卡片既读不动也滚不完。
    pub fn lists_cards(self) -> bool {
        matches!(self, Self::Day | Self::Week | Self::Month)
    }
}

/// 一个日历列：覆盖 `[start_day, end_day]`（含两端）的一段时间桶。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarBucket {
    /// 稳定标识，同时是字符串排序键；日刻度为当天，周刻度为该周周一。
    pub key: String,
    /// 展开态的列标题。
    pub label: String,
    /// 收起态（无卡片，宽度只有几个字符）用的短标题。
    pub short_label: String,
    pub start_day: String,
    pub end_day: String,
}

/// 卡片归属日未知时使用的桶键；这类卡片不能凭空从日历上消失。
pub const UNSCHEDULED_BUCKET_KEY: &str = "unscheduled";

/// 本地日历日字符串。
pub fn day_key_of(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// 今天的本地日历日。
pub fn today_key() -> String {
    day_key_of(Local::now().date_naive())
}

/// RFC3339 时间戳转本地日历日。
///
/// 后端时间戳是 UTC，直接截字符串会让东八区凌晨的卡片落到前一天，因此必须按本地时区换算。
/// 解析失败返回空串，调用方据此把卡片归入「未排期」而不是编一个日期出来。
pub fn day_key_from_iso(timestamp: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|value| day_key_of(value.with_timezone(&Local).date_naive()))
        .unwrap_or_default()
}

/// 日键转本地日历日。
///
/// 非规范输入（缺补零、不存在的日历日）一律返回 `None`：`chrono` 会拒绝 `2026-02-30`，
/// 但会收下 `2026-2-3`，放过去就会让日历把卡片放进一个标签对不上的列。
pub fn day_key_to_date(day_key: &str) -> Option<NaiveDate> {
    let bytes = day_key.as_bytes();
    let canonical = bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !canonical {
        return None;
    }
    NaiveDate::parse_from_str(day_key, "%Y-%m-%d").ok()
}

/// 卡片的归属日：优先用后端归一化过的 `date`，旧数据回落到创建日期。
pub fn card_day_key(card: &Card) -> String {
    if card.date.is_empty() {
        day_key_from_iso(&card.created_at)
    } else {
        card.date.clone()
    }
}

/// 卡片在指定刻度下所属的桶键；归属日无法确定时返回空串。
pub fn bucket_key_of(scale: CalendarScale, day_key: &str) -> String {
    let Some(date) = day_key_to_date(day_key) else {
        return String::new();
    };
    match scale {
        CalendarScale::Day => day_key.to_string(),
        CalendarScale::Week => day_key_of(start_of_week(date)),
        CalendarScale::Month => day_key[..7].to_string(),
        CalendarScale::Year => day_key[..4].to_string(),
    }
}

/// 指定刻度下要渲染的有序桶列表。
///
/// 覆盖范围随刻度逐级放大：日看当月、周看覆盖当月的整周、月看当年、年看当前十年。
/// 锚点日无法解析时返回空列表——渲染一个空日历比渲染一个错误的日历好。
pub fn buckets_for(scale: CalendarScale, anchor_day_key: &str) -> Vec<CalendarBucket> {
    let Some(anchor) = day_key_to_date(anchor_day_key) else {
        return Vec::new();
    };
    match scale {
        CalendarScale::Day => day_buckets(anchor),
        CalendarScale::Week => week_buckets(anchor),
        CalendarScale::Month => month_buckets(anchor),
        CalendarScale::Year => year_buckets(anchor),
    }
}

/// 按刻度把锚点前后翻一个周期。
///
/// 每个刻度只渲染当前周期内的桶，没有翻页，别的月份里的卡片就永远看不见。
/// 日/周刻度按整月翻，月刻度按整年翻，年刻度按整十年翻。
/// 锚点或翻页结果落到 `NaiveDate` 之外时原样返回，调用方拿到的仍是可渲染的锚点。
pub fn shift_anchor_day_key(scale: CalendarScale, anchor_day_key: &str, direction: i32) -> String {
    let Some(anchor) = day_key_to_date(anchor_day_key) else {
        return anchor_day_key.to_string();
    };
    let shifted = match scale {
        CalendarScale::Day | CalendarScale::Week => {
            shift_month(anchor.year(), anchor.month(), direction)
        },
        CalendarScale::Month => (anchor.year() + direction, 1),
        CalendarScale::Year => (anchor.year() + direction * 10, 1),
    };
    first_of_month(shifted.0, shifted.1)
        .map(day_key_of)
        .unwrap_or_else(|| anchor_day_key.to_string())
}

/// 日历头部显示的当前周期。
pub fn anchor_label(scale: CalendarScale, anchor_day_key: &str) -> String {
    let Some(anchor) = day_key_to_date(anchor_day_key) else {
        return String::new();
    };
    match scale {
        CalendarScale::Day | CalendarScale::Week => {
            format!("{} 年 {} 月", anchor.year(), anchor.month())
        },
        CalendarScale::Month => format!("{} 年", anchor.year()),
        CalendarScale::Year => {
            let first = decade_start(anchor.year());
            format!("{first} – {}", first + 9)
        },
    }
}

/// 归属日无法确定的卡片（旧数据），日历上必须给它们一个可见的落点。
pub fn unscheduled_cards(cards: &[Card]) -> Vec<&Card> {
    cards
        .iter()
        .filter(|card| card_day_key(card).is_empty())
        .collect()
}

/// 年月加减，结果仍落在 `1..=12` 月内。
fn shift_month(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let index = year * 12 + (month as i32 - 1) + delta;
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

fn first_of_month(year: i32, month: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(year, month, 1)
}

/// 某月的全部日期；年月超出 `NaiveDate` 范围时返回空列表。
fn days_of_month(year: i32, month: u32) -> Vec<NaiveDate> {
    let mut cursor = match first_of_month(year, month) {
        Some(first) => first,
        None => return Vec::new(),
    };
    let mut days = Vec::new();
    while cursor.month() == month {
        days.push(cursor);
        match cursor.succ_opt() {
            Some(next) => cursor = next,
            None => break,
        }
    }
    days
}

/// ISO 周：以周一为起点。
fn start_of_week(date: NaiveDate) -> NaiveDate {
    let offset = date.weekday().num_days_from_monday() as u64;
    date.checked_sub_days(Days::new(offset)).unwrap_or(date)
}

fn decade_start(year: i32) -> i32 {
    year.div_euclid(10) * 10
}

fn day_buckets(anchor: NaiveDate) -> Vec<CalendarBucket> {
    days_of_month(anchor.year(), anchor.month())
        .into_iter()
        .map(|date| {
            let key = day_key_of(date);
            CalendarBucket {
                label: format!("{}/{}", date.month(), date.day()),
                short_label: date.day().to_string(),
                start_day: key.clone(),
                end_day: key.clone(),
                key,
            }
        })
        .collect()
}

fn week_buckets(anchor: NaiveDate) -> Vec<CalendarBucket> {
    let days = days_of_month(anchor.year(), anchor.month());
    let (Some(first), Some(last)) = (days.first(), days.last()) else {
        return Vec::new();
    };
    let last_start = start_of_week(*last);

    let mut buckets = Vec::new();
    let mut cursor = start_of_week(*first);
    while cursor <= last_start {
        let Some(end) = cursor.checked_add_days(Days::new(6)) else {
            break;
        };
        let start_day = day_key_of(cursor);
        let end_day = day_key_of(end);
        buckets.push(CalendarBucket {
            key: start_day.clone(),
            label: format!(
                "{:02}/{:02}–{:02}/{:02}",
                cursor.month(),
                cursor.day(),
                end.month(),
                end.day()
            ),
            short_label: format!("{:02}/{:02}", cursor.month(), cursor.day()),
            start_day,
            end_day,
        });
        match cursor.checked_add_days(Days::new(7)) {
            Some(next) => cursor = next,
            None => break,
        }
    }
    buckets
}

fn month_buckets(anchor: NaiveDate) -> Vec<CalendarBucket> {
    let year = anchor.year();
    (1..=12)
        .filter_map(|month| {
            let first = first_of_month(year, month)?;
            let last = first.checked_add_months(Months::new(1))?.pred_opt()?;
            let key = format!("{year}-{month:02}");
            Some(CalendarBucket {
                key,
                label: format!("{month} 月"),
                short_label: format!("{month}月"),
                start_day: day_key_of(first),
                end_day: day_key_of(last),
            })
        })
        .collect()
}

fn year_buckets(anchor: NaiveDate) -> Vec<CalendarBucket> {
    let first_year = decade_start(anchor.year());
    (0..10)
        .filter_map(|index| {
            let year = first_year + index;
            let first = NaiveDate::from_ymd_opt(year, 1, 1)?;
            let last = NaiveDate::from_ymd_opt(year, 12, 31)?;
            Some(CalendarBucket {
                key: year.to_string(),
                label: format!("{year} 年"),
                short_label: year.to_string(),
                start_day: day_key_of(first),
                end_day: day_key_of(last),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;
    use crate::kanban::wire::CardColumn;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("测试日期必须合法")
    }

    fn card(date: &str, created_at: &str) -> Card {
        Card {
            id: "c".into(),
            title: "t".into(),
            body: String::new(),
            column: CardColumn::Backlog,
            working_dir: "/w".into(),
            session_id: None,
            attempt: 0,
            note: None,
            date: date.into(),
            created_at: created_at.into(),
            updated_at: created_at.into(),
        }
    }

    #[test]
    fn non_canonical_day_keys_are_rejected() {
        assert_eq!(day_key_to_date("2026-03-07"), Some(date(2026, 3, 7)));
        for rejected in [
            "2026-3-7",
            "2026-03-7",
            "2026-03-07T00:00:00Z",
            "2026-02-30",
            "2026-13-01",
            "",
        ] {
            assert_eq!(day_key_to_date(rejected), None, "{rejected} 必须被拒");
        }
    }

    #[test]
    fn day_scale_buckets_cover_every_day_of_a_leap_february() {
        let buckets = buckets_for(CalendarScale::Day, "2028-02-10");

        assert_eq!(buckets.len(), 29, "闰年二月是 29 天");
        assert_eq!(buckets[0].key, "2028-02-01");
        assert_eq!(buckets[0].label, "2/1");
        assert_eq!(buckets[0].short_label, "1");
        assert_eq!(buckets[28].key, "2028-02-29");
        for bucket in &buckets {
            assert_eq!(bucket.start_day, bucket.key);
            assert_eq!(bucket.end_day, bucket.key);
        }
    }

    #[test]
    fn week_scale_buckets_start_on_monday_and_span_the_whole_month() {
        // 2026-03-01 是周日，因此覆盖三月的整周要从 2026-02-23（周一）开始。
        let buckets = buckets_for(CalendarScale::Week, "2026-03-15");

        assert_eq!(buckets.len(), 6, "三月覆盖到 03-30 那一周为止");
        assert_eq!(buckets[0].key, "2026-02-23");
        assert_eq!(buckets[0].label, "02/23–03/01");
        assert_eq!(buckets[0].short_label, "02/23");
        assert_eq!(buckets[5].key, "2026-03-30");
        assert_eq!(buckets[5].end_day, "2026-04-05");
        for bucket in &buckets {
            assert_eq!(
                day_key_to_date(&bucket.key).unwrap().weekday(),
                chrono::Weekday::Mon
            );
        }
    }

    #[test]
    fn week_bucket_keys_are_mondays_for_every_day_of_the_week() {
        let monday = bucket_key_of(CalendarScale::Week, "2026-03-09");
        assert_eq!(monday, "2026-03-09");
        for day in [
            "2026-03-10",
            "2026-03-11",
            "2026-03-12",
            "2026-03-13",
            "2026-03-14",
            "2026-03-15",
        ] {
            assert_eq!(bucket_key_of(CalendarScale::Week, day), monday, "{day}");
        }
        assert_eq!(
            bucket_key_of(CalendarScale::Week, "2026-03-16"),
            "2026-03-16"
        );
    }

    #[test]
    fn month_and_year_bucket_keys_are_grounded_in_the_canonical_day_key() {
        assert_eq!(
            bucket_key_of(CalendarScale::Day, "2026-03-07"),
            "2026-03-07"
        );
        assert_eq!(bucket_key_of(CalendarScale::Month, "2026-03-07"), "2026-03");
        assert_eq!(bucket_key_of(CalendarScale::Year, "2026-03-07"), "2026");
        for scale in CalendarScale::ALL {
            assert_eq!(bucket_key_of(scale, "2026-3-7"), "", "{scale:?}");
        }
    }

    #[test]
    fn month_scale_buckets_cover_the_whole_year_with_real_end_days() {
        let buckets = buckets_for(CalendarScale::Month, "2026-03-15");

        assert_eq!(buckets.len(), 12);
        assert_eq!(buckets[0].key, "2026-01");
        assert_eq!(buckets[0].label, "1 月");
        assert_eq!(buckets[0].short_label, "1月");
        assert_eq!(buckets[0].end_day, "2026-01-31");
        assert_eq!(buckets[1].end_day, "2026-02-28");
        assert_eq!(buckets[11].end_day, "2026-12-31");
    }

    #[test]
    fn year_scale_buckets_cover_the_decade_and_do_not_list_cards() {
        let buckets = buckets_for(CalendarScale::Year, "2026-03-15");

        assert_eq!(buckets.len(), 10);
        assert_eq!(buckets[0].key, "2020");
        assert_eq!(buckets[0].start_day, "2020-01-01");
        assert_eq!(buckets[0].end_day, "2020-12-31");
        assert_eq!(buckets[9].key, "2029");
        assert!(!CalendarScale::Year.lists_cards());
        for scale in [
            CalendarScale::Day,
            CalendarScale::Week,
            CalendarScale::Month,
        ] {
            assert!(scale.lists_cards(), "{scale:?}");
        }
    }

    #[test]
    fn shifting_the_anchor_moves_by_whole_periods() {
        assert_eq!(
            shift_anchor_day_key(CalendarScale::Day, "2026-03-15", 1),
            "2026-04-01"
        );
        assert_eq!(
            shift_anchor_day_key(CalendarScale::Week, "2026-03-15", -1),
            "2026-02-01"
        );
        assert_eq!(
            shift_anchor_day_key(CalendarScale::Month, "2026-03-15", 1),
            "2027-01-01"
        );
        assert_eq!(
            shift_anchor_day_key(CalendarScale::Year, "2026-03-15", 1),
            "2036-01-01"
        );
    }

    /// 十二月往前翻要落在上一年的一月，而不是十三月。
    #[test]
    fn shifting_back_from_january_lands_in_the_previous_december() {
        assert_eq!(
            shift_anchor_day_key(CalendarScale::Day, "2026-01-10", -1),
            "2025-12-01"
        );
        assert_eq!(
            shift_anchor_day_key(CalendarScale::Month, "2026-01-10", -1),
            "2025-01-01"
        );
    }

    #[test]
    fn unparsable_anchors_yield_no_buckets_and_keep_their_label_empty() {
        assert!(buckets_for(CalendarScale::Month, "2026-3-15").is_empty());
        assert_eq!(anchor_label(CalendarScale::Month, "2026-3-15"), "");
        assert_eq!(
            shift_anchor_day_key(CalendarScale::Month, "2026-3-15", 1),
            "2026-3-15"
        );
    }

    #[test]
    fn anchor_labels_describe_the_rendered_period() {
        assert_eq!(
            anchor_label(CalendarScale::Day, "2026-03-15"),
            "2026 年 3 月"
        );
        assert_eq!(
            anchor_label(CalendarScale::Week, "2026-03-15"),
            "2026 年 3 月"
        );
        assert_eq!(anchor_label(CalendarScale::Month, "2026-03-15"), "2026 年");
        assert_eq!(
            anchor_label(CalendarScale::Year, "2026-03-15"),
            "2020 – 2029"
        );
    }

    #[test]
    fn card_day_key_prefers_the_normalized_date_and_falls_back_to_created_at() {
        assert_eq!(
            card_day_key(&card("2026-03-07", "2020-01-01T00:00:00+00:00")),
            "2026-03-07"
        );
        assert_eq!(
            card_day_key(&card("", "2026-03-07T12:00:00+00:00")),
            "2026-03-07"
        );
        assert_eq!(card_day_key(&card("", "not-a-timestamp")), "");
    }

    #[test]
    fn unscheduled_cards_are_the_ones_without_a_usable_day() {
        let cards = vec![
            card("2026-03-07", "2026-03-07T01:00:00+00:00"),
            card("", "not-a-timestamp"),
            card("", "2026-03-07T01:00:00+00:00"),
        ];

        let unscheduled = unscheduled_cards(&cards);

        assert_eq!(unscheduled.len(), 1);
        assert_eq!(unscheduled[0].created_at, "not-a-timestamp");
    }

    /// 归属日按本地时区换算，而不是直接截 UTC 日期。
    ///
    /// 本地凌晨在东半球时区已经跨到 UTC 的前一天，截 UTC 会把卡片排到昨天。
    #[test]
    fn day_key_from_iso_uses_the_local_calendar() {
        let local_after_midnight = Local
            .with_ymd_and_hms(2026, 6, 15, 0, 30, 0)
            .single()
            .expect("本地凌晨必须存在");

        assert_eq!(
            day_key_from_iso(&local_after_midnight.to_rfc3339()),
            "2026-06-15"
        );
        assert_eq!(day_key_from_iso("not-a-timestamp"), "");
    }

    #[test]
    fn today_key_is_a_canonical_day_key() {
        assert!(day_key_to_date(&today_key()).is_some());
    }

    /// 「回到今天」要把今天那一列滚进视口，靠的是在渲染出的桶里按桶键找到它的下标。
    ///
    /// 这里钉住「今天的桶键一定在今天的桶列表里」：一旦某个刻度的桶键换了算法而
    /// `bucket_key_of` 没跟上，按钮就会静静傀傀地什么都不滚。
    #[test]
    fn the_todays_bucket_key_is_always_one_of_the_rendered_buckets() {
        let today = today_key();

        for scale in CalendarScale::ALL {
            let buckets = buckets_for(scale, &today);
            let key = bucket_key_of(scale, &today);
            assert!(
                buckets.iter().any(|bucket| bucket.key == key),
                "{scale:?} 刻度下今天的桶键 {key} 不在渲染出的桶里"
            );
        }
    }
}

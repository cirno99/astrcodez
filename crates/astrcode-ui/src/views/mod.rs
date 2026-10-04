//! 视图层。

pub(crate) mod ask_user_card;
pub mod chat;
pub(crate) mod create_card;
pub mod kanban;
pub(crate) mod new_project;
pub(crate) mod project_path_field;
pub mod settings;
pub mod shell;
pub mod sidebar;

use astrcode_protocol::http::SessionListItemDto;
use gpui_kit::{
    AnyElement, App, Context, IntoElement, Styled as _,
    component::{
        ActiveTheme as _, Size,
        button::{Button, ButtonVariants as _},
        h_flex,
    },
};

use crate::icons::IconName;

/// 主区域显示哪一页。
///
/// 侧边栏的导航项与外壳的切换必须指的是同一组取值，因此放在两者之上，
/// 而不是各自定义一遍「当前在哪一页」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainView {
    Chat,
    Kanban,
    Settings,
}

/// 会话的显示名：标题为空时退回首条用户消息，再退回会话 id。
///
/// 侧边栏的行与聊天顶栏指的是同一个对象，它叫什么都只该有一处决定。名字一律压成一行：
/// 顶栏与横幅都按单行画（见 [`crate::session_list::single_line`]）。
pub(crate) fn display_title(item: &SessionListItemDto) -> String {
    if !item.title.trim().is_empty() {
        return crate::session_list::single_line(&item.title);
    }
    item.first_user_message
        .as_ref()
        .filter(|text| !text.trim().is_empty())
        .map(|text| crate::session_list::single_line(text))
        .unwrap_or_else(|| item.session_id.clone())
}

/// 页头的共用几何：四个区域同高，跨页切换时那条底边才停在同一像素上。
///
/// 横向内缩按主区的档给（`px_6`），侧边栏是导航列、窄一档，自己再盖一层。底边也归它画：
/// 相邻两层各画一条是同一条线画两遍。
pub(crate) fn page_header(cx: &App) -> gpui_kit::Div {
    h_flex()
        .flex_shrink_0()
        .items_center()
        .gap_2()
        .px_6()
        .h_12()
        .border_b_1()
        .border_color(cx.theme().border)
}

/// 页头与页脚的小图标按钮：没有可见文字，因此必须带一个可访问名——它同时就是悬停提示。
///
/// 走框架的 `Button` 而不是自绘的 div：图标按钮语义上仍是按钮，焦点环、按下态、提示浮层
/// 都该由它管；自绘版这三样一样都没有。
pub(crate) fn icon_button<T: 'static>(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    cx: &mut Context<T>,
    on_click: impl Fn(&mut T, &mut Context<T>) + 'static,
) -> AnyElement {
    Button::new(id)
        .ghost()
        .icon(icon.element(Size::Small))
        .tooltip(label)
        .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
        .into_any_element()
}

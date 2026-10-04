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
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _,
    component::{ActiveTheme as _, Size},
    div, px,
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

/// 页头与页脚的小图标按钮：32px 见方，悬停只换底色（前端是 `h-8 w-8 rounded-lg`）。
///
/// 悬停不换图标颜色：这版 gpui 没有 `group_hover`，子元素的颜色取不到父元素的悬停态。
pub(crate) fn icon_button<T: 'static>(
    id: &'static str,
    icon: IconName,
    cx: &mut Context<T>,
    on_click: impl Fn(&mut T, &mut Context<T>) + 'static,
) -> AnyElement {
    let hover_background = cx.theme().list_hover;
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(32.0))
        .rounded(cx.theme().radius)
        .text_color(cx.theme().muted_foreground)
        .hover(move |this| this.bg(hover_background))
        .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
        .child(icon.element(Size::Small))
        .into_any_element()
}

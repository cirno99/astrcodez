//! 可选择的一行代码：把「窗口级文本选择」接到按 token 上色的正文上。
//!
//! 框架自带的 `base::SelectableText` 已经实现了选择协议，但它内部画的是**不带高亮**的
//! `StyledText`：正文要逐 token 上色，直接用它会丢掉语法高亮。这里按同一套协议另写一个元素：
//! 注册选择 run、画选中底色、再画带高亮的文本；手势、跨行拼接与复制仍由框架的选择层负责
//! （窗口根节点里的 `TextSelectionLayer`，两个宿主都经 `gpui_kit::open_window` 装上）。

use std::ops::Range;

use gpui_kit::{
    App, BorderStyle, Bounds, Corners, Edges, Element, ElementId, GlobalElementId, HighlightStyle,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels,
    Point, SharedString, StyledText, TextLayout, TextStyle, Window,
    base::{TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun},
    component::ActiveTheme as _,
    transparent_black,
};

/// 一行可选择的代码。
///
/// 每一行是**独立的选择参与者**，句柄按元素 id 保留（见 [`Element::request_layout`]）：跨行复制
/// 由框架把各参与者的副本按 `order` 拼起来（`copy_items` 按 `document_order` 排序后换行相连）。
/// 全正文共用一条句柄反而不行：`update_runs` 是覆盖式的，后画的行会把先画的行顶掉。
pub(super) struct SelectableLine {
    id: ElementId,
    /// 这一行在正文里的次序：跨行复制按它拼选中的文本。
    order: u64,
    text: SharedString,
    /// 行内的高亮区间，按下标相对这一行起点。
    styles: Vec<(Range<usize>, HighlightStyle)>,
    /// 行文本的默认样式，高亮没盖到的空隙用它。
    default_style: TextStyle,
    /// 布局时建出来，`prepaint`/`paint` 复用同一份。
    styled: Option<StyledText>,
}

impl SelectableLine {
    pub(super) fn new(
        id: impl Into<ElementId>,
        order: u64,
        text: impl Into<SharedString>,
        styles: Vec<(Range<usize>, HighlightStyle)>,
        default_style: TextStyle,
    ) -> Self {
        Self {
            id: id.into(),
            order,
            text: text.into(),
            styles,
            default_style,
            styled: None,
        }
    }
}

impl IntoElement for SelectableLine {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelectableLine {
    type RequestLayoutState = TextSelectionHandle;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let handle = window.with_element_state(
            global_id.expect("可选择的行必须有稳定的元素 id"),
            |retained: Option<TextSelectionHandle>, _| {
                let handle =
                    retained.unwrap_or_else(|| TextSelectionHandle::new(self.text.clone(), cx));
                // 换了正文时行内容也变了，句柄记着的兜底文本跟着换；id 不变，选择状态照旧。
                handle.set_fallback_copy_text(self.text.clone(), cx);
                (handle.clone(), handle)
            },
        );

        let mut styled = StyledText::new(self.text.clone())
            .with_default_highlights(&self.default_style, self.styles.clone());
        let (layout_id, ()) = styled.request_layout(global_id, inspector_id, window, cx);
        self.styled = Some(styled);
        (layout_id, handle)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let styled = self.styled.as_mut().expect("request_layout 先于 prepaint");
        styled.prepaint(global_id, inspector_id, bounds, &mut (), window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let registration = TextSelectionRegistration::new(hitbox.clone(), bounds)
            .with_document_order(self.order)
            .with_text_bounds(vec![bounds])
            .with_rendered_element(handle, window, cx);
        handle.register(registration, window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let styled = self.styled.as_mut().expect("request_layout 先于 paint");
        let layout = styled.layout().clone();
        let selected_before = TextSelection::selected_text(window, cx);
        let projection = handle.update_runs(
            &[
                TextSelectionRun::new(self.text.clone(), layout.clone(), bounds)
                    .with_document_order(self.order),
            ],
            cx,
        );
        // 选中变化要立刻反映到本帧的底色上：复制走的是同一份投影，晚一帧就会复制到旧文本。
        if selected_before != TextSelection::selected_text(window, cx) {
            window.refresh();
        }
        let color = cx.theme().selection;
        for range in projection.ranges().iter().flatten().cloned() {
            paint_selection(&layout, range, color, window);
        }
        styled.paint(
            global_id,
            inspector_id,
            bounds,
            &mut (),
            &mut (),
            window,
            cx,
        );
    }
}

/// 画一段选中底色；跨行时首尾各截到行尾/行首，中间几行铺满。
fn paint_selection(layout: &TextLayout, range: Range<usize>, color: Hsla, window: &mut Window) {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return;
    };
    for bounds in selection_quads(start, end, layout.bounds(), layout.line_height()) {
        window.paint_quad(PaintQuad {
            bounds,
            background: color.into(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
            border_color: transparent_black(),
            border_style: BorderStyle::default(),
        });
    }
}

fn selection_quads(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        )];
    }

    let mut quads = vec![Bounds::from_corners(
        start,
        Point::new(bounds.right(), start.y + line_height),
    )];
    if end.y > start.y + line_height {
        quads.push(Bounds::from_corners(
            Point::new(bounds.left(), start.y + line_height),
            Point::new(bounds.right(), end.y),
        ));
    }
    quads.push(Bounds::from_corners(
        Point::new(bounds.left(), end.y),
        Point::new(end.x, end.y + line_height),
    ));
    quads
}

#[cfg(test)]
mod tests {
    use gpui_kit::{
        Context, Modifiers, MouseButton, ParentElement as _, Render, Styled as _, TestAppContext,
        base::TextSelectionLayer, div, point, px, size,
    };

    use super::*;

    #[test]
    fn a_wrapped_selection_paints_full_width_middle_lines() {
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(100.), px(100.)));
        let quads = selection_quads(
            point(px(40.), px(20.)),
            point(px(30.), px(80.)),
            bounds,
            px(20.),
        );
        assert_eq!(
            quads,
            vec![
                Bounds::from_corners(point(px(40.), px(20.)), point(px(110.), px(40.))),
                Bounds::from_corners(point(px(10.), px(40.)), point(px(110.), px(80.))),
                Bounds::from_corners(point(px(10.), px(80.)), point(px(30.), px(100.))),
            ]
        );
    }
    /// 拖动跨过两行时，选中的文本应是两行拼起来的那一段。
    ///
    /// 这是「正文可选中」的最低要求：编译通过只能证明元素画得出来，证明不了选择层真的收下了
    /// 各行注册的 run，也证明不了两行按 `order` 拼成了同一份文档。
    #[gpui_kit::test]
    fn a_drag_across_two_lines_selects_both(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        let (_, cx) = cx.add_window_view(|_, _| TwoLines);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_mouse_down(
            point(px(1.), px(8.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(px(80.), px(30.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(80.), px(30.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
            let text = TextSelection::selected_text(window, cx);
            assert!(
                text.contains("alpha") && text.contains("beta"),
                "跨行拖动应选中两行，实际选中 {text:?}"
            );
        });
    }

    /// 两行各是一条选择参与者，按行序拼成一份可选文档。
    struct TwoLines;

    impl Render for TwoLines {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let style = window.text_style();
            div().size_full().child(TextSelectionLayer).child(
                div()
                    .w(px(240.))
                    .child(SelectableLine::new(
                        "line-0",
                        0,
                        "alpha",
                        Vec::new(),
                        style.clone(),
                    ))
                    .child(SelectableLine::new("line-1", 1, "beta", Vec::new(), style)),
            )
        }
    }
}

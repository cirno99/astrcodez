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

/// 一行可选择的代码；同一个 `handle` 上的多行按 `order` 拼成一份可选文档。
pub(super) struct SelectableLine {
    id: ElementId,
    handle: TextSelectionHandle,
    /// 这一行在正文里的次序：跨行选择按它拼接选中的文本。
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
        handle: TextSelectionHandle,
        order: u64,
        text: impl Into<SharedString>,
        styles: Vec<(Range<usize>, HighlightStyle)>,
        default_style: TextStyle,
    ) -> Self {
        Self {
            id: id.into(),
            handle,
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
        let mut styled = StyledText::new(self.text.clone())
            .with_default_highlights(&self.default_style, self.styles.clone());
        let (layout_id, ()) = styled.request_layout(global_id, inspector_id, window, cx);
        self.styled = Some(styled);
        (layout_id, self.handle.clone())
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
    use gpui_kit::{point, px, size};

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
}

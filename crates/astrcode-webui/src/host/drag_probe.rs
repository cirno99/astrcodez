//! 拖拽探针：回答「自绘拖拽在 wasm 宿主上能不能做」。
//!
//! 与浮层探针同一套路：不连服务端、固定几何、纯色块。四段各演一种拖拽形态，
//! 验收脚本按颜色找包围盒，判据是区域之间的关系（谁落进了谁的盒子、哪条边移了多少）。
//!
//! 色块对照（`.consult/webui-spike/e2e/drag-probe.mjs` 里同名）：
//!
//! - 深红列一 / 深蓝列一：场景一，黄卡从左边拖到右边（跨容器投递）
//! - 青面板 / 紫把手：场景二，拖把手改面板宽度（侧栏宽度拖拽的类比）
//! - 深红列二 / 深蓝列二：场景三，橙卡与洋红卡成组拖拽（白条是选中标记）
//! - 绿松卡：场景四，原生指针回退（`on_mouse_down/move/up`，不走框架拖拽）
//! - 淡紫蓝：拖拽时跟着指针画的幽灵，宽度随载荷里的卡片数变宽

use std::collections::HashSet;

use gpui_kit::{
    AppContext as _, Context, DragMoveEvent, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Render, StatefulInteractiveElement as _,
    Styled as _, Window,
    component::{h_flex, v_flex},
    div, px, rgb,
};

/// 场景三组里的第一张卡（橙）。
const CARD_ONE: u8 = 1;
/// 场景三组里的第二张卡（洋红）。
const CARD_TWO: u8 = 2;

/// 侧栏宽度的可拖范围，与 `astrcode-ui` 的侧栏同一量级。
const MIN_PANEL_WIDTH: f32 = 80.0;
const MAX_PANEL_WIDTH: f32 = 400.0;
/// 场景四里卡片的位移上限，够验收脚本读出「拖了多远」。
const MAX_POINTER_OFFSET: f32 = 140.0;

/// 场景一的载荷：被拖的就是一张卡，没有别的信息。
struct CardDrag;

/// 场景三的载荷：一次拖的是一组卡片，组里的每一张都跟着走。
#[derive(Clone)]
struct GroupDrag {
    cards: Vec<u8>,
}

/// 场景二的载荷：只用来把这次拖拽与卡片拖拽区分开。
struct ResizeDrag;

/// 拖拽时跟着指针的幽灵。宽度为 0 表示这次拖拽不画预览（resize 用不到）。
struct DragGhost {
    width: f32,
}

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(self.width)).h(px(40.)).bg(rgb(0x8080ff))
    }
}

pub(crate) struct DragProbe {
    card_dropped: bool,
    panel_width: f32,
    resize_origin: Option<(f32, f32)>,
    group_selected: bool,
    group_dropped: HashSet<u8>,
    pointer_offset: f32,
    pointer_origin: Option<(f32, f32)>,
}

impl DragProbe {
    pub(crate) fn new() -> Self {
        Self {
            card_dropped: false,
            panel_width: 200.,
            resize_origin: None,
            group_selected: false,
            group_dropped: HashSet::new(),
            pointer_offset: 0.,
            pointer_origin: None,
        }
    }
}

impl Render for DragProbe {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut card_source = div()
            .id("drag-probe-card-source")
            .w(px(200.))
            .h(px(120.))
            .p_2()
            .bg(rgb(0x802020));
        if !self.card_dropped {
            card_source = card_source.child(
                div()
                    .id("drag-probe-card")
                    .w(px(60.))
                    .h(px(40.))
                    .bg(rgb(0xffff00))
                    .on_drag(CardDrag, |_, _, _, cx| cx.new(|_| DragGhost { width: 60. })),
            );
        }

        let mut card_target = div()
            .id("drag-probe-card-target")
            .w(px(200.))
            .h(px(120.))
            .p_2()
            .bg(rgb(0x202080))
            .on_drop(cx.listener(|this, _: &CardDrag, _, cx| {
                this.card_dropped = true;
                cx.notify();
            }));
        if self.card_dropped {
            card_target = card_target.child(div().w(px(60.)).h(px(40.)).bg(rgb(0x00ff00)));
        }

        let row_cards = h_flex().gap_4().child(card_source).child(card_target);

        let resize_row = h_flex()
            .id("drag-probe-resize-row")
            .h(px(120.))
            .on_drag_move::<ResizeDrag>(cx.listener(
                |this, event: &DragMoveEvent<ResizeDrag>, _, cx| {
                    // 起点记在按下那一刻：第一次 `drag_move` 到达时指针已经走了十几像素，
                    // 拿它当起点会把这十几像素吃掉（真实 resize 同样不该这么算）。
                    let Some((origin_x, origin_width)) = this.resize_origin else {
                        return;
                    };
                    let x = f32::from(event.event.position.x);
                    this.panel_width =
                        (origin_width + x - origin_x).clamp(MIN_PANEL_WIDTH, MAX_PANEL_WIDTH);
                    cx.notify();
                },
            ))
            .on_drop(cx.listener(|this, _: &ResizeDrag, _, cx| {
                this.resize_origin = None;
                cx.notify();
            }))
            .child(div().w(px(self.panel_width)).h(px(120.)).bg(rgb(0x00ffff)))
            .child(
                div()
                    .id("drag-probe-resize-handle")
                    .w(px(12.))
                    .h(px(120.))
                    .bg(rgb(0x8000ff))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, _| {
                            this.resize_origin =
                                Some((f32::from(event.position.x), this.panel_width));
                        }),
                    )
                    .on_drag(ResizeDrag, |_, _, _, cx| {
                        cx.new(|_| DragGhost { width: 0. })
                    }),
            );

        let group_payload = if self.group_selected {
            vec![CARD_ONE, CARD_TWO]
        } else {
            vec![CARD_ONE]
        };

        let mut group_source = v_flex()
            .id("drag-probe-group-source")
            .w(px(200.))
            .h(px(120.))
            .p_2()
            .gap_2()
            .bg(rgb(0x402020));
        if self.group_selected {
            group_source = group_source.child(div().w_full().h(px(6.)).bg(rgb(0xffffff)));
        }
        for (card, color, element_id) in [
            (CARD_ONE, 0xff8000, "drag-probe-group-card-one"),
            (CARD_TWO, 0xff00ff, "drag-probe-group-card-two"),
        ] {
            if self.group_dropped.contains(&card) {
                continue;
            }
            let cards = group_payload.clone();
            group_source = group_source.child(
                div()
                    .id(element_id)
                    .w(px(60.))
                    .h(px(40.))
                    .bg(rgb(color))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.group_selected = !this.group_selected;
                        cx.notify();
                    }))
                    .on_drag(GroupDrag { cards }, move |drag, _, _, cx| {
                        let width = 60. * drag.cards.len() as f32;
                        cx.new(|_| DragGhost { width })
                    }),
            );
        }

        let mut group_target = v_flex()
            .id("drag-probe-group-target")
            .w(px(200.))
            .h(px(120.))
            .p_2()
            .gap_2()
            .bg(rgb(0x202040))
            .on_drop(cx.listener(|this, drag: &GroupDrag, _, cx| {
                for card in &drag.cards {
                    this.group_dropped.insert(*card);
                }
                cx.notify();
            }));
        if self.group_dropped.contains(&CARD_ONE) {
            group_target = group_target.child(div().w(px(60.)).h(px(40.)).bg(rgb(0xff8000)));
        }
        if self.group_dropped.contains(&CARD_TWO) {
            group_target = group_target.child(div().w(px(60.)).h(px(40.)).bg(rgb(0xff00ff)));
        }

        let row_group = h_flex().gap_4().child(group_source).child(group_target);

        let pointer_row = h_flex()
            .id("drag-probe-pointer-row")
            .w(px(420.))
            .h(px(60.))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let Some((origin_x, origin_offset)) = this.pointer_origin else {
                    return;
                };
                let x = f32::from(event.position.x);
                this.pointer_offset = (origin_offset + x - origin_x).clamp(0., MAX_POINTER_OFFSET);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.pointer_origin = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("drag-probe-pointer-card")
                    .ml(px(self.pointer_offset))
                    .w(px(60.))
                    .h(px(40.))
                    .bg(rgb(0x00ff80))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.pointer_origin =
                                Some((f32::from(event.position.x), this.pointer_offset));
                            cx.notify();
                        }),
                    ),
            );

        v_flex()
            .size_full()
            .p_2()
            .gap_2()
            .bg(rgb(0x101010))
            .child(row_cards)
            .child(resize_row)
            .child(row_group)
            .child(pointer_row)
    }
}

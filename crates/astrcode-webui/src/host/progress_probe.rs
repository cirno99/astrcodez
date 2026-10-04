//! 进度条探针（临时，诊断用）：复刻 todoWrite 卡片的展开结构。
//!
//! 点击"展开"后出现若干 `Progress`，用于验证展开后重绘是否停不下来。
//! 诊断完成后整个文件连同入口一起删除。

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window,
    component::{Sizable as _, h_flex, progress::Progress, v_flex},
    px, rgb,
};

pub(crate) struct ProgressProbe {
    open: bool,
}

impl ProgressProbe {
    pub(crate) fn new() -> Self {
        Self { open: false }
    }
}

impl Render for ProgressProbe {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = v_flex()
            .size_full()
            .p_4()
            .gap_2()
            .bg(rgb(0x101014))
            .text_color(rgb(0xe6e6e6));

        root = root.child(
            h_flex()
                .id("progress-probe-toggle")
                .items_center()
                .w(px(200.))
                .h(px(40.))
                .px_2()
                .rounded(px(6.))
                .bg(rgb(0x202030))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.open = !this.open;
                    cx.notify();
                }))
                .child(if self.open { "收起" } else { "展开" }),
        );

        if self.open {
            let mut list = v_flex().gap_2().w(px(400.));
            for index in 0..6 {
                list = list.child(
                    v_flex()
                        .gap_1()
                        .child(h_flex().gap_2().child(format!("任务 {index}")))
                        .child(
                            Progress::new(format!("probe-progress-{index}"))
                                .value([0.0, 50.0, 100.0][index % 3])
                                .color(rgb(0x7aa2f7))
                                .small(),
                        ),
                );
            }
            root = root.child(list);
        }

        root
    }
}

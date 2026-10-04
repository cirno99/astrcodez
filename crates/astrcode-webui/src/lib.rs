//! astrcode Web UI 宿主（wasm）。
//!
//! 与 `astrcode-gui` 共用同一个 `astrcode-ui`，差别只在宿主注入：
//!
//! - HTTP 客户端由浏览器提供（`WebPlatform::fetch_http_client`），原生宿主注入 reqwest。
//! - `base_url` 留空，请求按相对引用落到页面自身的 origin —— 页面由 astrcode server
//!   同源托管，同源即同服务端，宿主因此不需要知道自己的地址。
//! - `working_dir` 交空串，由服务端解析成它自己的启动目录：浏览器无从得知服务端 cwd。
//!
//! 只在 wasm32 上成立。原生目标下这个 crate 是空的，仅为让 workspace 的
//! `cargo check --all-targets` 通过而存在。

#[cfg(target_family = "wasm")]
mod host {
    use std::{borrow::Cow, cell::RefCell, rc::Rc, sync::Arc};

    use astrcode_ui::{api::Api, views::shell::Shell};
    use gpui_kit::{
        App, AppContext as _, Application, IntoElement, ParentElement as _, Styled as _,
        WindowOptions,
        component::{button::Button, popover::Popover, theme::Theme, v_flex},
        deferred, div, px, rgb,
    };
    use wasm_bindgen::prelude::*;

    mod drag_probe;

    /// 进度条探针（临时，诊断用）。
    mod progress_probe;

    thread_local! {
        /// wasm 单线程；句柄只是为了让 `Application` 不被 drop。
        static APPLICATION: RefCell<Option<gpui_kit::ApplicationHandle>> =
            const { RefCell::new(None) };
    }

    /// 浏览器入口。页面 origin 就是服务端，无需参数。
    #[wasm_bindgen]
    pub fn run() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        let _ = console_log::init_with_level(log::Level::Warn);

        gpui_kit::platform::web_init();

        let platform = Rc::new(
            gpui_kit::web::WebPlatform::new_with_backend_and_font_fallback(
                false,
                gpui_kit::web::WebBackendPreference::Auto,
                // 浏览器里没有系统字体，CJK 字形缺口只能靠它本地字体兜底。
                gpui_kit::web::CanvasFontFallback::EmojiAndCjk,
            ),
        );
        let http_client = Arc::new(platform.fetch_http_client());
        let app = Application::with_platform(platform)
            .with_http_client(http_client)
            .with_assets(gpui_kit::assets::Assets::new("/"));

        let launch = move |cx: &mut App| {
            gpui_kit::init(cx);
            configure(cx);

            gpui_kit::open_window(WindowOptions::default(), cx, move |window, cx| {
                let api = Api::new(String::new(), cx.http_client());
                cx.new(|cx| Shell::new(api, String::new(), window, cx))
            })
            .expect("打开窗口失败");
            cx.activate(true);
        };

        APPLICATION.with(|application| {
            *application.borrow_mut() = Some(app.run_embedded(launch));
        });
        Ok(())
    }

    /// 进度条探针入口（临时，诊断用）。
    #[wasm_bindgen]
    pub fn run_progress_probe() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        let _ = console_log::init_with_level(log::Level::Warn);

        gpui_kit::platform::web_init();

        let platform = Rc::new(
            gpui_kit::web::WebPlatform::new_with_backend_and_font_fallback(
                false,
                gpui_kit::web::WebBackendPreference::Auto,
                gpui_kit::web::CanvasFontFallback::EmojiAndCjk,
            ),
        );
        let app = Application::with_platform(platform);

        let launch = move |cx: &mut App| {
            gpui_kit::init(cx);
            configure(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, move |_window, cx| {
                cx.new(|_cx| progress_probe::ProgressProbe::new())
            })
            .expect("打开窗口失败");
            cx.activate(true);
        };

        APPLICATION.with(|application| {
            *application.borrow_mut() = Some(app.run_embedded(launch));
        });
        Ok(())
    }

    /// 供页面确认 wasm 已装载。
    #[wasm_bindgen]
    pub fn ping() -> String {
        "astrcode-webui".into()
    }

    /// 浮层探针入口：不连服务端，只回答「延迟绘制的浮层在 wasm 宿主上画不画得出来」。
    ///
    /// 每块都是固定几何 + 纯色，验收脚本按颜色在整幅画面上找包围盒就能判定，
    /// 不需要知道任何坐标。
    #[wasm_bindgen]
    pub fn run_probe() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        let _ = console_log::init_with_level(log::Level::Warn);

        gpui_kit::platform::web_init();

        let platform = Rc::new(
            gpui_kit::web::WebPlatform::new_with_backend_and_font_fallback(
                false,
                gpui_kit::web::WebBackendPreference::Auto,
                gpui_kit::web::CanvasFontFallback::EmojiAndCjk,
            ),
        );
        let http_client = Arc::new(platform.fetch_http_client());
        let app = Application::with_platform(platform)
            .with_http_client(http_client)
            .with_assets(gpui_kit::assets::Assets::new("/"));

        let launch = move |cx: &mut App| {
            gpui_kit::init(cx);
            configure(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, move |_window, cx| {
                cx.new(|_cx| ProbeView { overlay: false })
            })
            .expect("打开窗口失败");
            cx.activate(true);
        };

        APPLICATION.with(|application| {
            *application.borrow_mut() = Some(app.run_embedded(launch));
        });
        Ok(())
    }

    /// 拖拽探针入口：不连服务端，只回答「自绘拖拽在 wasm 宿主上做不做得了」。
    #[wasm_bindgen]
    pub fn run_drag_probe() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        let _ = console_log::init_with_level(log::Level::Warn);

        gpui_kit::platform::web_init();

        let platform = Rc::new(
            gpui_kit::web::WebPlatform::new_with_backend_and_font_fallback(
                false,
                gpui_kit::web::WebBackendPreference::Auto,
                gpui_kit::web::CanvasFontFallback::EmojiAndCjk,
            ),
        );
        let http_client = Arc::new(platform.fetch_http_client());
        let app = Application::with_platform(platform)
            .with_http_client(http_client)
            .with_assets(gpui_kit::assets::Assets::new("/"));

        let launch = move |cx: &mut App| {
            gpui_kit::init(cx);
            configure(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, move |_window, cx| {
                cx.new(|_cx| drag_probe::DragProbe::new())
            })
            .expect("打开窗口失败");
            cx.activate(true);
        };

        APPLICATION.with(|application| {
            *application.borrow_mut() = Some(app.run_embedded(launch));
        });
        Ok(())
    }

    /// 宿主侧的字体与主题装配。
    ///
    /// 产品观感（模式、调色板、圆角）由 [`astrcode_ui::theme::install`] 决定，两个宿主同一份；
    /// 这里只剩浏览器独有的那一件事：字体。
    fn configure(cx: &mut App) {
        // web 文本系统把 `.SystemUIFont` 落在族名 `IBM Plex Sans` 上，且自己不提供任何
        // 字体：这个族没人占住，第一帧之后的文本测量就会 panic，所以拉丁字形随包带一份。
        // 非拉丁字形不走这里，由 Canvas 的系统字体兜底（`CanvasFontFallback::EmojiAndCjk`），
        // 浏览器里的中文因此用的是本机字体。
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../assets/IBMPlexSans-Regular.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/JetBrainsMono-Regular.ttf").as_slice()),
            ])
            .expect("加载字体失败");
        // 字体先于主题：切换模式时 `Theme::change` 会解析字体族，那时字体得已经在册。
        astrcode_ui::theme::install(cx);
        // 只钉 monospace：主题默认的 mono 是探测本机等宽族得到的，浏览器里一个族都探测不到。
        // sans 不钉——它保持主题默认的 `.SystemUIFont`，与原生宿主同一个语义。
        Theme::update(cx, |theme| {
            theme.mono_font_family = "JetBrains Mono".into();
        });
    }

    /// 探针视图：五种铺法各占一块固定几何的纯色，看谁画得出来。
    ///
    /// - 红：普通绝对定位（控制组）
    /// - 绿盖红：绝对定位 + 延迟绘制叠上去
    /// - 青：延迟绘制直接排在流里
    /// - 蓝：组件浮层 `Popover`（默认打开，不需要点击）
    /// - 洋红：点一下才出现黄色自绘浮层的容器
    struct ProbeView {
        overlay: bool,
    }

    impl gpui_kit::Render for ProbeView {
        fn render(
            &mut self,
            _window: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl IntoElement {
            let size = px(200.);
            let height = px(60.);
            let red = || {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(rgb(0xff0000))
            };

            let mut root = v_flex().size_full().gap_1().p_2().bg(rgb(0x101010));

            // 控制在同形状的盒子里，只有绘制路径不一样。
            root = root.child(div().relative().w(size).h(height).child(red()));
            root = root.child(
                div()
                    .relative()
                    .w(size)
                    .h(height)
                    .child(red())
                    .child(deferred(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(rgb(0x00ff00)),
                    )),
            );
            root = root.child(deferred(div().w(size).h(height).bg(rgb(0x00ffff))));

            // 点击区排在浮层行之前：`Popover` 的内容落在触发器下方，压住它就无法点击。
            let mut toggle = div()
                .relative()
                .w(px(420.))
                .h(height)
                .bg(rgb(0xff00ff))
                .child(Button::new("probe-toggle").label("开").on_click(
                    cx.listener(|this, _, _, cx| {
                        this.overlay = !this.overlay;
                        log::warn!("probe-click overlay={}", this.overlay);
                        cx.notify();
                    }),
                ))
                // 状态指示器：始终在场，白 = 标志位为真。
                .child(
                    div()
                        .absolute()
                        .left(px(120.))
                        .top_0()
                        .w(px(40.))
                        .h(px(60.))
                        .bg(if self.overlay {
                            rgb(0xffffff)
                        } else {
                            rgb(0x202020)
                        }),
                );
            if self.overlay {
                toggle = toggle.child(
                    div()
                        .absolute()
                        .left(px(220.))
                        .top_0()
                        .w(px(200.))
                        .h(px(60.))
                        .bg(rgb(0xffff00)),
                );
            }
            root = root.child(toggle);

            root.child(
                Popover::new("probe-popover")
                    .default_open(true)
                    .trigger(Button::new("probe-trigger").label("触发器"))
                    .content(|_, _, _| {
                        div()
                            .w(px(200.))
                            .h(px(60.))
                            .bg(rgb(0x0000ff))
                            .into_any_element()
                    }),
            )
        }
    }
}

//! 产品主题：两个宿主的唯一主题出处。
//!
//! 视图一律按语义角色取色（`cx.theme().primary`、`.border`、`.muted_foreground`……），
//! 所以「产品长什么样」只在这里决定一次。此前主题散在宿主里：Web 宿主钉了深色模式，
//! 桌面宿主一个都没设；两边取的还都是 gpui-kit 自带的灰阶调色板（深色下 `primary`
//! 是纯白），同一份视图在两个宿主上都对不上产品的身份。
//!
//! 调色板照搬前端 `index.css` 的 `[data-theme='dark']` 段，与那套 UI 同源。裸色值只允许
//! 出现在这里：视图侧一律读角色，不再各自写死颜色。
//!
//! 装配走 [`Theme::apply_config`]，也就是框架装载主题文件走的那条路，而不是装载之后再逐个
//! 改语义色。组件级令牌（`button_primary`、`popover` 等）是在装载时由语义色推导并落定的，
//! 事后改语义色不会带动它们：主按钮会留在框架的白色上，浮层也还是框架的底色。走
//! `apply_config` 则整张表一起重算，没写的字段按本模式的内置主题继承。
//!
//! 宿主差异留在宿主里——Web 宿主得自带字体是因为浏览器没有系统字体，那是宿主的能力差异，
//! 不是产品观感，因此不进本模块。

use std::rc::Rc;

use gpui_kit::{
    App, Hsla,
    component::theme::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode},
    rgb,
};

/// 装配产品主题。两个宿主都在 `gpui_kit::init` 之后、开窗之前调用它。
pub fn install(cx: &mut App) {
    let config = Rc::new(product_config());
    Theme::update(cx, |theme| theme.apply_config(&config));
}

/// 品牌块的底色与字色（前端 `bg-[#a8ad83]`）。
///
/// 它是产品标识而不是任何语义状态，所以不进主题角色表，只在这里定义一次供视图取用。
pub(crate) fn brand_avatar_background() -> Hsla {
    rgb(0xa8ad83).into()
}

/// 品牌块上的字色。
pub(crate) fn brand_avatar_foreground() -> Hsla {
    rgb(0xffffff).into()
}

/// 产品主题配置；只写产品说了算的角色，其余交给本模式的内置主题。
///
/// 状态色（`danger` / `warning` / `success` / `info`）只覆盖背景一侧，它们的
/// `*_foreground` 保持内置主题的关系：产品色与前端的语义色同族，原关系在对比度上依然成立。
/// 悬停与按下也交给框架推导（它按底色与角色的比例算），少一批手挑的近似值。
fn product_config() -> ThemeConfig {
    // `ThemeConfigColors` 的基色（red/green/blue…）是私有字段，结构体更新语法用不了，
    // 因此逐个赋值。
    let mut colors = ThemeConfigColors::default();
    // ── 基础层：底色、正文、边界 ──
    colors.background = Some("#121313".into()); // --app-bg
    colors.foreground = Some("#f1f1f1".into()); // --text-primary
    colors.border = Some("#ffffff14".into()); // --border（白 8%）
    colors.input = Some("#ffffff14".into()); // 输入框边界与 --border 同值
    colors.caret = Some("#f1f1f1".into());

    // ── 次级与弱化 ──
    colors.muted = Some("#ffffff14".into()); // --surface-muted
    colors.muted_foreground = Some("#949494".into()); // --text-muted

    // ── 强调：产品的紫，用于悬停底色与选中填充 ──
    colors.accent = Some("#a99fd029".into()); // --accent-soft
    colors.accent_foreground = Some("#bdb4e0".into()); // --accent-strong
    colors.primary = Some("#b3a9d8".into()); // --btn-primary-bg
    colors.primary_foreground = Some("#17171a".into()); // --btn-primary-fg

    // ── 抬升面：面板与次级按钮 ──
    colors.secondary = Some("#2e2e2f".into()); // --surface
    colors.secondary_foreground = Some("#f1f1f1".into());

    // ── 容器与浮层 ──
    colors.group_box = Some("#1d1d1e".into()); // --surface-soft
    colors.group_box_foreground = Some("#f1f1f1".into());
    colors.popover = Some("#2e2e2f".into());
    colors.popover_foreground = Some("#f1f1f1".into());

    // ── 列表与选中 ──
    colors.list = Some("#1d1d1e".into());
    colors.list_hover = Some("#ffffff14".into());
    colors.list_active = Some("#a99fd029".into());

    // ── 侧边栏 ──
    colors.sidebar = Some("#202524".into()); // --sidebar-bg
    colors.sidebar_foreground = Some("#f1f1f1".into());
    colors.sidebar_border = Some("#ffffff14".into());
    colors.sidebar_accent = Some("#ffffff14".into());
    colors.sidebar_accent_foreground = Some("#f1f1f1".into());
    colors.sidebar_primary = Some("#b3a9d8".into());
    colors.sidebar_primary_foreground = Some("#17171a".into());

    // ── 状态色 ──
    colors.danger = Some("#f87171".into());
    colors.warning = Some("#fbbf24".into());
    colors.success = Some("#4ade80".into());
    colors.info = Some("#38bdf8".into()); // --phase-calling-tool

    // ── 链接 ──
    colors.link = Some("#b0a6d6".into()); // --link

    // ── 交互与滚动 ──
    colors.ring = Some("#a99fd07a".into()); // --shadow-focus-accent 的紫
    colors.selection = Some("#a99fd059".into());
    colors.scrollbar_thumb = Some("#ffffff29".into());
    colors.scrollbar_thumb_hover = Some("#525252".into());

    ThemeConfig {
        name: "AstrCode".into(),
        mode: ThemeMode::Dark,
        // 圆角按前端同档：控件 8、弹窗 12。
        radius: Some(8),
        radius_lg: Some(12),
        colors,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui_kit::{
        Hsla, Rgba,
        component::theme::{Theme, ThemeMode},
        px, rgb,
    };

    use super::product_config;

    /// 装一遍产品主题，拿到装载后的整张表。
    fn installed() -> Theme {
        let mut theme = Theme::default();
        theme.apply_config(&Rc::new(product_config()));
        theme
    }

    /// 产品身份不能被 gpui-kit 自带的灰阶调色板顶掉：深色主题下 `primary` 是纯白，
    /// 那会让主按钮长成框架的示例样式，而不是产品的紫。
    #[test]
    fn product_theme_replaces_the_stock_grayscale() {
        let theme = installed();

        assert_eq!(theme.mode, ThemeMode::Dark);
        assert_eq!(theme.background, Hsla::from(rgb(0x121313)));
        assert_eq!(theme.foreground, Hsla::from(rgb(0xf1f1f1)));
        assert_eq!(theme.sidebar, Hsla::from(rgb(0x202524)));
        assert_eq!(theme.radius, px(8.));
        assert_eq!(theme.radius_lg, px(12.));
    }

    /// 组件级令牌必须在装载时从产品色推导出来，而不是留着框架的旧值。
    ///
    /// 这条是「装载后再改语义色」那个坑的守卫：那样改，主按钮会停在白色上，
    /// 浮层也还是框架的底色，与产品色对不上。
    #[test]
    fn component_tokens_follow_the_product_colors() {
        let theme = installed();
        let primary = theme.primary;

        assert_eq!(theme.tokens.button_primary.color, primary);
        assert_eq!(theme.button_primary, primary);
        assert_eq!(theme.button_primary_foreground, theme.primary_foreground);
        assert_eq!(theme.tokens.popover.color, theme.popover);
        assert_ne!(theme.tokens.button_primary.color, Hsla::from(rgb(0xfafafa)));
    }

    /// 可读性是调色板的底线，改色时得有东西拦住低对比度的取色。
    ///
    /// 断言的是 WCAG 2.1 的相对亮度比（AA：正文 4.5:1），取的是产品真的会画在一起的那几对。
    #[test]
    fn text_pairs_keep_wcag_aa_contrast() {
        let theme = installed();

        for (pair, text, surface) in [
            ("正文 / 底色", theme.foreground, theme.background),
            ("元信息 / 底色", theme.muted_foreground, theme.background),
            ("正文 / 抬升面", theme.foreground, theme.secondary),
            (
                "主按钮字 / 主按钮底",
                theme.primary_foreground,
                theme.primary,
            ),
            ("错误文字 / 底色", theme.danger, theme.background),
        ] {
            let ratio = contrast_ratio(text, surface);
            assert!(ratio >= 4.5, "{pair} 的对比度只有 {ratio:.2}∶1");
        }
    }

    /// WCAG 2.1 的相对亮度比：亮的一侧加 0.05 除以暗的一侧加 0.05。
    fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// WCAG 2.1 的相对亮度：sRGB 分量先线性化，再按人眼敏感度加权。
    fn relative_luminance(color: Hsla) -> f32 {
        let rgba = Rgba::from(color);
        let linear = |channel: f32| {
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
    }
}

//! 界面图标：形状照搬 Web 前端的 `components/ui/Icon.tsx`。
//!
//! 图标字节留在共享 UI 层、由 [`Icon::data`] 直接渲染，不交给宿主的资产源按路径取：
//! 两个宿主注册的资产源覆盖的目录不同（桌面宿主用 gpui-kit 的默认图标捆绑，Web 宿主
//! 走 HTTP 按需拉取），按路径寻址会让同一处界面在两端缺图。字节编进二进制也意味着
//! 图标没有「先空一拍、下载完再补」的第一帧。
//!
//! 颜色不在这里决定：gpui 的 SVG 渲染只取 alpha 通道，图标实际颜色永远是元素上的
//! 文本色，所以调用点用 `.text_color(...)` 指定。

use gpui_kit::component::{Icon, Sizable as _, Size};

/// 24×24 描边图标的外壳。图形本体逐条照搬前端，外壳只写一次。
macro_rules! svg {
    ($body:literal) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round">"#,
            $body,
            "</svg>"
        )
        .as_bytes()
    };
}

/// 图标名，与前端 `ui/Icon.tsx` 的 `IconName` 逐一对应。
///
/// 整份图标集随前端一次搬完；暂时没有调用点的变体等各自界面迁移到位后自然被引用。
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum IconName {
    Sidebar,
    Send,
    Close,
    ChevronRight,
    ArrowLeft,
    Plug,
    Folder,
    Project,
    Edit,
    Settings,
    Users,
    Copy,
    Retry,
    Refresh,
    Recap,
    ChevronDown,
    Trash,
    Plus,
    Shield,
    Monitor,
    Branch,
    Spark,
    Board,
    Check,
    Brain,
    Compact,
    Zap,
    Terminal,
}

impl IconName {
    /// 全部图标，供测试逐个过一遍。
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::Sidebar,
        Self::Send,
        Self::Close,
        Self::ChevronRight,
        Self::ArrowLeft,
        Self::Plug,
        Self::Folder,
        Self::Project,
        Self::Edit,
        Self::Settings,
        Self::Users,
        Self::Copy,
        Self::Retry,
        Self::Refresh,
        Self::Recap,
        Self::ChevronDown,
        Self::Trash,
        Self::Plus,
        Self::Shield,
        Self::Monitor,
        Self::Branch,
        Self::Spark,
        Self::Board,
        Self::Check,
        Self::Brain,
        Self::Compact,
        Self::Zap,
        Self::Terminal,
    ];

    /// 给定尺寸的图标元素；颜色交给调用点。
    pub(crate) fn element(self, size: Size) -> Icon {
        Icon::default().data(self.svg()).with_size(size)
    }

    fn svg(self) -> &'static [u8] {
        match self {
            Self::Sidebar => svg!(
                r#"<rect x="3" y="3" width="18" height="18" rx="2" ry="2" stroke-width="2"/><line x1="9" y1="3" x2="9" y2="21" stroke-width="2"/>"#
            ),
            Self::Send => svg!(
                r#"<line x1="12" y1="19" x2="12" y2="5" stroke-width="2.5"/><polyline points="5 12 12 5 19 12" stroke-width="2.5"/>"#
            ),
            Self::Close => svg!(
                r#"<line x1="18" y1="6" x2="6" y2="18" stroke-width="2"/><line x1="6" y1="6" x2="18" y2="18" stroke-width="2"/>"#
            ),
            Self::ChevronRight => svg!(r#"<polyline points="9 18 15 12 9 6" stroke-width="2"/>"#),
            Self::ArrowLeft => svg!(
                r#"<line x1="19" y1="12" x2="5" y2="12" stroke-width="2.5"/><polyline points="12 5 5 12 12 19" stroke-width="2.5"/>"#
            ),
            Self::Plug => svg!(
                r#"<path d="M12 22v-5" stroke-width="2"/><path d="M9 8V2" stroke-width="2"/><path d="M15 8V2" stroke-width="2"/><path d="M6 8h12v3a6 6 0 0 1-12 0V8Z" stroke-width="2"/>"#
            ),
            // 这一枚是 20×20 的格子，线也细一档，与前端一致。
            Self::Folder => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 5.75A1.75 1.75 0 0 1 4.25 4h4.03c.46 0 .9.18 1.23.5l1.02 1c.32.3.74.47 1.18.47h4.04A1.75 1.75 0 0 1 17.5 7.72v6.53A1.75 1.75 0 0 1 15.75 16H4.25A1.75 1.75 0 0 1 2.5 14.25V5.75Z" stroke-width="1.4"/></svg>"#,
            Self::Project => svg!(
                r#"<rect x="5" y="4" width="14" height="16" rx="2" stroke-width="2"/><path d="M9 8h6" stroke-width="2"/><path d="M9 12h6" stroke-width="2"/><path d="M9 16h4" stroke-width="2"/>"#
            ),
            Self::Edit => svg!(
                r#"<path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" stroke-width="1.5"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z" stroke-width="1.5"/>"#
            ),
            // 实心图标：只有填充，不带描边。
            Self::Settings => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor" stroke-linecap="round" stroke-linejoin="round"><path d="M10.4 2h3.2l.5 2.6c.6.2 1.1.5 1.6.9l2.5-.9 1.6 2.8-2 1.7c.1.3.1.6.1.9s0 .6-.1.9l2 1.7-1.6 2.8-2.5-.9c-.5.4-1 .7-1.6.9l-.5 2.6h-3.2l-.5-2.6c-.6-.2-1.1-.5-1.6-.9l-2.5.9-1.6-2.8 2-1.7c-.1-.3-.1-.6-.1-.9s0-.6.1-.9l-2-1.7 1.6-2.8 2.5.9c.5-.4 1-.7 1.6-.9L10.4 2Zm1.6 6.5A3.5 3.5 0 1 0 12 15.5 3.5 3.5 0 0 0 12 8.5Z"/></svg>"#,
            Self::Users => svg!(
                r#"<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" stroke-width="2"/><circle cx="9" cy="7" r="4" stroke-width="2"/><path d="M22 21v-2a4 4 0 0 0-3-3.87" stroke-width="2"/><path d="M16 3.13a4 4 0 0 1 0 7.75" stroke-width="2"/>"#
            ),
            Self::Copy => svg!(
                r#"<rect x="9" y="9" width="13" height="13" rx="2" stroke-width="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" stroke-width="2"/>"#
            ),
            Self::Retry => svg!(
                r#"<path d="M1 4v6h6" stroke-width="2"/><path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10" stroke-width="2"/>"#
            ),
            Self::Refresh => svg!(
                r#"<path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" stroke-width="2"/><path d="M21 3v5h-5" stroke-width="2"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" stroke-width="2"/><path d="M8 16H3v5" stroke-width="2"/>"#
            ),
            Self::Recap => svg!(
                r#"<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8l-6-6Z" stroke-width="2" stroke-linejoin="round"/><path d="M14 2v6h6" stroke-width="2" stroke-linejoin="round"/><path d="M9 13h6" stroke-width="2" stroke-linecap="round"/><path d="M9 17h6" stroke-width="2" stroke-linecap="round"/>"#
            ),
            Self::ChevronDown => svg!(r#"<polyline points="6 9 12 15 18 9" stroke-width="2"/>"#),
            Self::Trash => svg!(
                r#"<polyline points="3 6 5 6 21 6" stroke-width="2"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" stroke-width="2"/><path d="M10 11v6" stroke-width="2"/><path d="M14 11v6" stroke-width="2"/><path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" stroke-width="2"/>"#
            ),
            Self::Plus => svg!(
                r#"<path d="M12 5v14" stroke-width="2"/><path d="M5 12h14" stroke-width="2"/>"#
            ),
            Self::Shield => svg!(
                r#"<path d="M12 3 19 6v5c0 4.5-2.8 8.4-7 10-4.2-1.6-7-5.5-7-10V6l7-3Z" stroke-width="2"/><path d="m9 12 2 2 4-4" stroke-width="2"/>"#
            ),
            Self::Monitor => svg!(
                r#"<rect x="3" y="4" width="18" height="13" rx="2" stroke-width="2"/><path d="M8 21h8" stroke-width="2"/><path d="M12 17v4" stroke-width="2"/>"#
            ),
            Self::Branch => svg!(
                r#"<circle cx="6" cy="18" r="3" stroke-width="2"/><circle cx="18" cy="6" r="3" stroke-width="2"/><path d="M6 15V5" stroke-width="2"/><path d="M6 5h6a6 6 0 0 1 6 6v-2" stroke-width="2"/>"#
            ),
            Self::Spark => svg!(
                r#"<path d="M12 3.5c1.7 0 2.6 1.1 3.1 2.3 1.3-.1 2.7.4 3.6 1.6.9 1.2.9 2.7.4 3.9.9.9 1.4 2.3.9 3.7-.5 1.4-1.6 2.3-2.9 2.6-.4 1.2-1.4 2.3-2.9 2.5-1.5.2-2.8-.5-3.5-1.5-1.2.4-2.7.2-3.7-.9-1-1-1.3-2.5-.9-3.7-1.1-.7-1.8-1.9-1.7-3.4.1-1.5 1-2.6 2.1-3.2.2-1.3 1.1-2.5 2.5-3 1.4-.5 2.8 0 3.7.8.4-1 1.5-1.7 2.8-1.7Z" stroke-width="1.8"/><path d="m9 9 2.8 3L9 15" stroke-width="1.8"/><path d="M13.5 15H16" stroke-width="1.8"/>"#
            ),
            Self::Board => svg!(
                r#"<rect x="3" y="4" width="18" height="16" rx="2" stroke-width="2"/><path d="M9 4v16" stroke-width="2"/><path d="M15 4v16" stroke-width="2"/><path d="M5 8h2" stroke-width="2"/><path d="M11 8h2" stroke-width="2"/><path d="M17 8h2" stroke-width="2"/>"#
            ),
            Self::Check => svg!(r#"<path d="m5 12 5 5L19 7" stroke-width="2.4"/>"#),
            Self::Brain => svg!(
                r#"<path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z" stroke-width="2"/><path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z" stroke-width="2"/><path d="M15 13a4.5 4.5 0 0 1-3-4 4.5 4.5 0 0 1-3 4" stroke-width="2"/>"#
            ),
            Self::Compact => svg!(
                r#"<polyline points="1 4 1 10 7 10" stroke-width="2"/><polyline points="23 20 23 14 17 14" stroke-width="2"/><path d="M20.49 9A9 9 0 0 0 5.64 5.64L1 10m22 4l-4.64 4.36A9 9 0 0 1 3.51 15" stroke-width="2"/>"#
            ),
            Self::Zap => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor" stroke-linecap="round" stroke-linejoin="round"><path d="M13 10V3L4 14h7v7l9-11h-7z"/></svg>"#,
            Self::Terminal => svg!(
                r#"<path d="M8 8 4 12l4 4M16 8l4 4-4 4M13 5l-2 14" stroke-width="2"/>"#
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui_kit::SvgRenderer;

    use super::IconName;

    #[test]
    fn every_icon_carries_a_parsable_svg_document() {
        for name in IconName::ALL {
            let svg = std::str::from_utf8(name.svg()).expect("图标是 UTF-8");
            assert!(svg.starts_with("<svg "), "{name:?} 缺少根元素");
            assert!(svg.ends_with("</svg>"), "{name:?} 未闭合");
            // 尺寸来源：usvg 从 viewBox 推 intrinsic size，缺了它就渲染不出像素。
            assert!(svg.contains("viewBox="), "{name:?} 缺少 viewBox");
            // gpui 只取 alpha 通道，颜色必须由描边或填充给出。
            assert!(
                svg.contains("stroke=\"currentColor\"") || svg.contains("fill=\"currentColor\""),
                "{name:?} 既无描边也无填充"
            );
        }
    }

    /// 用真正上屏的那个解析器过一遍全部图标。
    ///
    /// 形状写错在浏览器里只表现为「少一枚图」，控制台也没有信号；这一条把它变成红测试。
    #[test]
    fn every_icon_parses_with_the_renderer_that_paints_it() {
        let renderer = SvgRenderer::new(Arc::new(gpui_kit::assets::Assets::new("/")));
        for name in IconName::ALL {
            if let Err(error) = renderer.parse_svg(name.svg()) {
                panic!("{name:?} 无法解析：{error}");
            }
        }
    }
}

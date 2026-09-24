use ratatui::style::Color;

/// 主题：界面与 Markdown 渲染的全部配色槽位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub text: Color,
    pub dim: Color,
    pub accent: Color,
    pub h1: Color,
    pub h2: Color,
    pub h3: Color,
    pub h4: Color,
    pub h5: Color,
    pub h6: Color,
    /// 行内代码前景
    pub code_fg: Color,
    /// 行内代码/代码块兜底背景
    pub code_bg: Color,
    /// 引用块竖线
    pub quote: Color,
    /// 表格边框/水平分割线
    pub border: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    /// 搜索命中的高亮配色
    pub search_bg: Color,
    pub search_fg: Color,
    /// 对应的 syntect 高亮主题名
    pub code_theme: &'static str,
    /// 语法高亮前景提亮百分比（浅色主题为 0）
    pub code_lift: u8,
}

pub const DARK: Theme = Theme {
    name: "dark",
    text: Color::White,
    dim: Color::DarkGray,
    accent: Color::Cyan,
    h1: Color::Cyan,
    h2: Color::Cyan,
    h3: Color::White,
    h4: Color::Gray,
    h5: Color::Gray,
    h6: Color::DarkGray,
    code_fg: Color::White,
    code_bg: Color::DarkGray,
    quote: Color::DarkGray,
    border: Color::DarkGray,
    status_bg: Color::DarkGray,
    status_fg: Color::White,
    search_bg: Color::Rgb(250, 189, 47),
    search_fg: Color::Black,
    code_theme: "base16-eighties.dark",
    code_lift: 18,
};

pub const GRUVBOX: Theme = Theme {
    name: "gruvbox",
    text: Color::Rgb(235, 219, 178),
    dim: Color::Rgb(146, 131, 116),
    accent: Color::Rgb(250, 189, 47),
    h1: Color::Rgb(251, 73, 52),
    h2: Color::Rgb(254, 128, 25),
    h3: Color::Rgb(250, 189, 47),
    h4: Color::Rgb(184, 187, 38),
    h5: Color::Rgb(142, 192, 124),
    h6: Color::Rgb(131, 165, 152),
    code_fg: Color::Rgb(235, 219, 178),
    code_bg: Color::Rgb(60, 56, 54),
    quote: Color::Rgb(146, 131, 116),
    border: Color::Rgb(80, 73, 69),
    status_bg: Color::Rgb(60, 56, 54),
    status_fg: Color::Rgb(235, 219, 178),
    search_bg: Color::Rgb(250, 189, 47),
    search_fg: Color::Rgb(60, 56, 54),
    code_theme: "Solarized (dark)",
    code_lift: 18,
};

pub const CATPPUCCIN: Theme = Theme {
    name: "catppuccin",
    text: Color::Rgb(205, 214, 244),
    dim: Color::Rgb(108, 112, 134),
    accent: Color::Rgb(203, 166, 247),
    h1: Color::Rgb(243, 139, 168),
    h2: Color::Rgb(250, 179, 135),
    h3: Color::Rgb(249, 226, 175),
    h4: Color::Rgb(166, 227, 161),
    h5: Color::Rgb(148, 226, 213),
    h6: Color::Rgb(137, 180, 250),
    code_fg: Color::Rgb(205, 214, 244),
    code_bg: Color::Rgb(30, 30, 46),
    quote: Color::Rgb(108, 112, 134),
    border: Color::Rgb(88, 91, 112),
    status_bg: Color::Rgb(30, 30, 46),
    status_fg: Color::Rgb(205, 214, 244),
    search_bg: Color::Rgb(249, 226, 175),
    search_fg: Color::Rgb(30, 30, 46),
    code_theme: "base16-mocha.dark",
    code_lift: 15,
};

pub const DRACULA: Theme = Theme {
    name: "dracula",
    text: Color::Rgb(248, 248, 242),
    dim: Color::Rgb(98, 114, 164),
    accent: Color::Rgb(189, 147, 249),
    h1: Color::Rgb(255, 121, 198),
    h2: Color::Rgb(189, 147, 249),
    h3: Color::Rgb(139, 233, 253),
    h4: Color::Rgb(80, 250, 123),
    h5: Color::Rgb(241, 250, 140),
    h6: Color::Rgb(255, 184, 108),
    code_fg: Color::Rgb(248, 248, 242),
    code_bg: Color::Rgb(40, 42, 54),
    quote: Color::Rgb(98, 114, 164),
    border: Color::Rgb(68, 71, 90),
    status_bg: Color::Rgb(40, 42, 54),
    status_fg: Color::Rgb(248, 248, 242),
    search_bg: Color::Rgb(241, 250, 140),
    search_fg: Color::Rgb(40, 42, 54),
    code_theme: "base16-seti.dark",
    code_lift: 15,
};

pub const NORD: Theme = Theme {
    name: "nord",
    text: Color::Rgb(216, 222, 233),
    dim: Color::Rgb(76, 86, 106),
    accent: Color::Rgb(136, 192, 208),
    h1: Color::Rgb(136, 192, 208),
    h2: Color::Rgb(129, 161, 193),
    h3: Color::Rgb(216, 222, 233),
    h4: Color::Rgb(216, 222, 233),
    h5: Color::Rgb(216, 222, 233),
    h6: Color::Rgb(76, 86, 106),
    code_fg: Color::Rgb(216, 222, 233),
    code_bg: Color::Rgb(46, 52, 64),
    quote: Color::Rgb(76, 86, 106),
    border: Color::Rgb(76, 86, 106),
    status_bg: Color::Rgb(46, 52, 64),
    status_fg: Color::Rgb(216, 222, 233),
    search_bg: Color::Rgb(235, 203, 139),
    search_fg: Color::Rgb(46, 52, 64),
    code_theme: "base16-ocean.dark",
    code_lift: 18,
};

pub const ALL: [Theme; 5] = [DARK, GRUVBOX, NORD, DRACULA, CATPPUCCIN];

impl Theme {
    /// 解析 `--theme` 参数；缺省 dark，未知名称报错并列出可用值。
    pub fn by_name(name: Option<&str>) -> anyhow::Result<Self> {
        let name = name.unwrap_or("dark");
        ALL.iter()
            .copied()
            .find(|t| t.name == name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "未知主题 `{name}`，可用主题: {}",
                    ALL.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
                )
            })
    }

    /// 按终端能力解析最终配色：Rgb 颜色降级到 256/16 色，NO_COLOR 时全部退为默认色。
    pub fn resolved(self) -> Self {
        if crate::term::no_color() {
            let r = Color::Reset;
            return Theme {
                name: self.name,
                text: r,
                dim: r,
                accent: r,
                h1: r,
                h2: r,
                h3: r,
                h4: r,
                h5: r,
                h6: r,
                code_fg: r,
                code_bg: r,
                quote: r,
                border: r,
                status_bg: r,
                status_fg: r,
                search_bg: r,
                search_fg: r,
                code_theme: self.code_theme,
                code_lift: self.code_lift,
            };
        }
        let a = crate::term::adapt;
        Theme {
            name: self.name,
            text: a(self.text),
            dim: a(self.dim),
            accent: a(self.accent),
            h1: a(self.h1),
            h2: a(self.h2),
            h3: a(self.h3),
            h4: a(self.h4),
            h5: a(self.h5),
            h6: a(self.h6),
            code_fg: a(self.code_fg),
            code_bg: a(self.code_bg),
            quote: a(self.quote),
            border: a(self.border),
            status_bg: a(self.status_bg),
            status_fg: a(self.status_fg),
            search_bg: a(self.search_bg),
            search_fg: a(self.search_fg),
            code_theme: self.code_theme,
            code_lift: self.code_lift,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_resolves_all_and_defaults_to_dark() {
        assert_eq!(Theme::by_name(None).unwrap(), DARK);
        for t in ALL {
            assert_eq!(Theme::by_name(Some(t.name)).unwrap(), t);
        }
    }

    #[test]
    fn by_name_rejects_unknown() {
        assert!(Theme::by_name(Some("solarized")).is_err());
    }
}

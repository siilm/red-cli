use ratatui::style::{Color, Modifier, Style};
use syntect::easy::HighlightLines;
use syntect::highlighting::FontStyle;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// syntect 封装：代码块 → 按主题配色的 (Style, text) 行片段。
/// 实例化开销较大（加载默认语法/主题集），应在应用生命周期内复用。
pub struct Highlighter {
    ps: SyntaxSet,
    theme: syntect::highlighting::Theme,
    /// 前景提亮百分比（深色主题 > 0，浅色主题为 0）
    lift: u8,
}

impl Highlighter {
    pub fn new(syntect_theme: &str, lift: u8) -> Self {
        let ps = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let theme = ts
            .themes
            .get(syntect_theme)
            .cloned()
            .or_else(|| ts.themes.values().next().cloned())
            .expect("syntect 默认主题集非空");
        Self { ps, theme, lift }
    }

    /// 高亮主题的背景色（用作代码块整行底色），按终端能力降级。
    pub fn background(&self) -> Option<Color> {
        self.theme
            .settings
            .background
            .map(|c| crate::term::adapt(Color::Rgb(c.r, c.g, c.b)))
    }

    /// 按语言高亮一段代码；未知语言回退纯文本。返回每个物理行的高亮片段。
    pub fn highlight(&self, code: &str, lang: Option<&str>) -> Vec<Vec<(Style, String)>> {
        let syntax = lang
            .and_then(|t| self.ps.find_syntax_by_token(t))
            .unwrap_or_else(|| self.ps.find_syntax_plain_text());
        let mut hl = HighlightLines::new(syntax, &self.theme);
        let mut out: Vec<Vec<(Style, String)>> = Vec::new();
        for line in code.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let Ok(ranges) = hl.highlight_line(line, &self.ps) else {
                out.push(Vec::new());
                continue;
            };
            let spans: Vec<(Style, String)> = ranges
                .into_iter()
                .map(|(st, s)| (convert(st, self.lift), s.to_string()))
                .filter(|(_, s)| !s.is_empty())
                .collect();
            out.push(spans);
        }
        // 末尾换行产生的空行去掉一个
        if out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out
    }
}

fn convert(st: syntect::highlighting::Style, lift: u8) -> Style {
    let c = st.foreground;
    let fg = crate::term::adapt(Color::Rgb(
        lift_channel(c.r, lift),
        lift_channel(c.g, lift),
        lift_channel(c.b, lift),
    ));
    let mut s = Style::new().fg(fg);
    if st.font_style.contains(FontStyle::BOLD) {
        s = s.add_modifier(Modifier::BOLD);
    }
    if st.font_style.contains(FontStyle::ITALIC) {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if st.font_style.contains(FontStyle::UNDERLINE) {
        s = s.add_modifier(Modifier::UNDERLINED);
    }
    s
}

/// 将颜色通道向白色提升 lift%，改善深色主题下语法高亮发暗的问题。
fn lift_channel(v: u8, lift: u8) -> u8 {
    v.saturating_add(((255 - v) as u16 * u16::from(lift) / 100) as u8)
}

#[cfg(test)]
mod tests {
    use super::lift_channel;

    #[test]
    fn lift_channel_brightens_and_caps() {
        assert!(lift_channel(40, 18) > 40);
        assert_eq!(lift_channel(255, 18), 255);
        assert_eq!(lift_channel(0, 0), 0);
        assert_eq!(lift_channel(200, 0), 200);
    }
}

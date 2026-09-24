use ratatui::layout::{Constraint, Layout, Margin, Rect};
#[cfg(feature = "native-images")]
use ratatui::layout::Size;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
#[cfg(feature = "native-images")]
use ratatui_image::Image;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, ImagePanel, Mode};
use crate::browser::{BrowserEntry, human_size};
use crate::markdown::{Document, ImageHit, TocEntry};
use crate::theme::Theme;

pub const IMAGE_PANEL_WIDTH: u16 = 36;

#[derive(Debug, Clone, Copy)]
pub struct LayoutModel {
    pub document: Rect,
    pub side: Option<Rect>,
    pub image_panel: Option<Rect>,
    pub status: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibleRowKind {
    Content,
    CollapsedSummary,
}

#[derive(Debug, Clone)]
pub struct VisibleRow {
    pub line: Line<'static>,
    pub source_line: Option<usize>,
    /// 由折叠展开视图额外添加的缩进列数。
    pub extra_indent: usize,
    pub kind: VisibleRowKind,
}

pub fn layout_for(area: Rect, app: &App) -> LayoutModel {
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let mut main = rows[0];
    let mut side = None;
    let mut image_panel = None;
    match app.mode {
        Mode::Toc => {
            let cols = Layout::horizontal([Constraint::Min(1), Constraint::Length(30)]).split(main);
            main = cols[0];
            side = Some(cols[1]);
        }
        Mode::Settings | Mode::ZoneInput => {
            let cols = Layout::horizontal([Constraint::Min(1), Constraint::Length(30)]).split(main);
            main = cols[0];
            side = Some(cols[1]);
        }
        Mode::Read if !matches!(app.image_panel, ImagePanel::Closed) => {
            let panel_width = IMAGE_PANEL_WIDTH.min(main.width.saturating_sub(1));
            let cols = Layout::horizontal([
                Constraint::Min(1),
                Constraint::Length(panel_width),
            ])
            .split(main);
            main = cols[0];
            image_panel = Some(cols[1]);
        }
        _ => {}
    }
    LayoutModel {
        document: inset(main, app),
        side,
        image_panel,
        status: rows[1],
    }
}

pub fn document_width(width: u16, app: &App) -> u16 {
    layout_for(Rect::new(0, 0, width, 2), app).document.width.max(20)
}

#[cfg(feature = "native-images")]
pub fn native_image_size(app: &App) -> Option<Size> {
    let height = app.view_height.saturating_add(1).min(usize::from(u16::MAX)) as u16;
    let panel = layout_for(Rect::new(0, 0, app.render_width, height), app).image_panel?;
    Some(Size::new(
        panel.width.saturating_sub(1).max(1),
        panel.height.saturating_sub(5).max(1),
    ))
}

pub fn draw(f: &mut Frame, app: &App) {
    let layout = layout_for(f.area(), app);
    if let Some(area) = layout.side {
        match app.mode {
            Mode::Toc => draw_toc(f, app, area),
            Mode::Settings | Mode::ZoneInput => draw_settings(f, app, area),
            _ => {}
        }
    }
    match app.mode {
        Mode::Browse => draw_browser(f, app, layout.document),
        _ => render_document(f, app, layout.document),
    }
    if let Some(area) = layout.image_panel {
        draw_image_panel(f, app, area);
    }
    f.render_widget(status_bar(app, f.area().width), layout.status);
}

/// 左右安全区内缩。
fn inset(area: Rect, app: &App) -> Rect {
    if app.safe_zone > 0 {
        area.inner(Margin::new(u16::from(app.safe_zone), 0))
    } else {
        area
    }
}

fn render_document(f: &mut Frame, app: &App, area: Rect) {
    let view_h = area.height as usize;
    let query = app.search_query.as_deref();
    let visible: Vec<Line<'static>> = visible_rows(
        &app.doc.lines,
        &app.doc.collapsibles,
        &app.folds,
        app.scroll,
        view_h,
        app.theme,
    )
    .into_iter()
    .map(|row| match query {
        Some(q) => highlight_line(&row.line, q, app.theme),
        None => row.line,
    })
    .collect();
    f.render_widget(
        Paragraph::new(visible).style(Style::new().fg(app.theme.text)),
        area,
    );
}

/// 折叠与滚动后的可见行模型。绘制和图片命中测试都使用这组行，避免
/// 两套折叠/缩进映射逐渐产生偏差。
pub fn visible_rows(
    lines: &[Line<'_>],
    collapsibles: &[crate::markdown::Collapsible],
    folds: &[bool],
    scroll: usize,
    view_h: usize,
    theme: Theme,
) -> Vec<VisibleRow> {
    let folded_at = |i: usize| folds.get(i).copied().unwrap_or(false);
    let ranges: Vec<(usize, usize)> = collapsibles
        .iter()
        .enumerate()
        .filter(|&(i, c)| folded_at(i) && c.end > c.line)
        .map(|(_, c)| (c.line, c.end))
        .collect();
    let hidden = |r: usize| ranges.iter().any(|&(s, e)| s < r && r < e);
    let summary_of = |r: usize| {
        collapsibles
            .iter()
            .enumerate()
            .find(|&(i, c)| folded_at(i) && c.line == r && c.end > c.line)
    };
    let indent_of = |r: usize| {
        collapsibles
            .iter()
            .filter(|c| c.line < r && r < c.end)
            .count()
            .min(4)
    };

    let mut start = scroll.min(lines.len());
    if hidden(start)
        && let Some(c) = collapsibles.iter().find(|c| c.line < start && start < c.end)
    {
        start = c.line;
    }

    let mut out = Vec::with_capacity(view_h);
    let mut r = start;
    while out.len() < view_h && r < lines.len() {
        if hidden(r) {
            r += 1;
            continue;
        }
        if let Some((_, c)) = summary_of(r) {
            let n = c.end - c.line - 1;
            out.push(VisibleRow {
                line: Line::from(Span::styled(
                    format!("▸ {}（{} 行）", c.title, n),
                    Style::new().fg(theme.accent),
                )),
                source_line: None,
                extra_indent: 0,
                kind: VisibleRowKind::CollapsedSummary,
            });
            r = c.end.max(r + 1);
            continue;
        }
        let depth = indent_of(r);
        let mut spans: Vec<Span<'static>> = Vec::new();
        if depth > 0 {
            spans.push(Span::raw("  ".repeat(depth)));
        }
        spans.extend(
            lines[r]
                .spans
                .iter()
                .map(|s| Span::styled(s.content.to_string(), s.style)),
        );
        out.push(VisibleRow {
            line: Line::from(spans).style(lines[r].style),
            source_line: Some(r),
            extra_indent: depth * 2,
            kind: VisibleRowKind::Content,
        });
        r += 1;
    }
    out
}

#[cfg(test)]
fn apply_folds(
    lines: &[Line<'_>],
    collapsibles: &[crate::markdown::Collapsible],
    folds: &[bool],
    scroll: usize,
    view_h: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    visible_rows(lines, collapsibles, folds, scroll, view_h, theme)
        .into_iter()
        .map(|row| row.line)
        .collect()
}

/// 在当前阅读视图中精确命中图片的 alt/path 文字。
/// 坐标以终端左上角为原点；状态栏、右侧栏、折叠隐藏区和普通空白均不命中。
pub fn image_hit_at(app: &App, x: u16, y: u16) -> Option<ImageHit> {
    if app.mode != Mode::Read {
        return None;
    }
    let height = app.view_height.saturating_add(1).min(usize::from(u16::MAX)) as u16;
    let layout = layout_for(Rect::new(0, 0, app.render_width, height), app);
    let area = layout.document;
    if x < area.x || x >= area.right() || y < area.y || y >= area.bottom() {
        return None;
    }
    let row_idx = usize::from(y - area.y);
    let row = visible_rows(
        &app.doc.lines,
        &app.doc.collapsibles,
        &app.folds,
        app.scroll,
        area.height as usize,
        app.theme,
    )
    .into_iter()
    .nth(row_idx)?;
    if row.kind != VisibleRowKind::Content {
        return None;
    }
    let source_line = row.source_line?;
    let col = usize::from(x - area.x);
    let source_col = col.checked_sub(row.extra_indent)?;
    app.doc
        .image_hits
        .iter()
        .find(|hit| {
            hit.line == source_line
                && hit.start_col <= source_col
                && source_col < hit.end_col
                && app.doc.images.get(hit.image).is_some()
        })
        .cloned()
}

fn draw_image_panel(f: &mut Frame, app: &App, area: Rect) {
    let ImagePanel::PathOnly { image } = &app.image_panel else {
        return;
    };
    let mut lines = vec![
        Line::from(Span::styled(
            format!("图片 #{}", image.id),
            Style::new().fg(app.theme.accent).add_modifier(Modifier::BOLD),
        )),
        Line::raw(String::new()),
        Line::from(format!("原始路径: {}", image.raw_path)),
    ];
    match &image.resolved_path {
        Some(path) => lines.push(Line::from(format!("解析路径: {}", path.display()))),
        None if image.is_local => lines.push(Line::raw("解析路径: (无 Markdown 文件路径)")),
        None => lines.push(Line::raw("解析路径: (远程 URI 未解析)")),
    }

    #[cfg(feature = "native-images")]
    let native_loaded = app.native_image.as_ref().is_some_and(|cache| {
        cache.image_id == image.id && cache.raw_path == image.raw_path
    });
    #[cfg(not(feature = "native-images"))]
    let native_loaded = false;

    if image.is_local {
        lines.push(Line::raw(if native_loaded {
            "状态: 原生图片协议"
        } else {
            "状态: PathOnly（原生图片支持未启用或加载失败）"
        }));
    } else {
        lines.push(Line::raw("错误: 仅支持本地图片，不加载远程 URI"));
    }

    #[cfg(feature = "native-images")]
    if native_loaded
        && let Some(cache) = app.native_image.as_ref()
    {
        let block = side_panel_block(" 图片 (Native) ", app.theme);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let text_height = (lines.len() as u16).min(inner.height);
        let rows = Layout::vertical([
            Constraint::Length(text_height),
            Constraint::Min(1),
        ])
        .split(inner);
        if text_height > 0 {
            f.render_widget(
                Paragraph::new(lines).style(Style::new().fg(app.theme.text)),
                rows[0],
            );
        }
        if rows[1].height > 0 && rows[1].width > 0 {
            f.render_widget(Image::new(&cache.protocol).allow_clipping(true), rows[1]);
        }
        return;
    }

    f.render_widget(
        Paragraph::new(lines)
            .style(Style::new().fg(app.theme.text))
            .block(side_panel_block(" 图片 (PathOnly) ", app.theme)),
        area,
    );
}

pub fn status_text(app: &App, width: u16) -> String {
    let budget = ((width as usize) / 3).clamp(12, 40);
    if app.mode == Mode::Browse {
        return format!(
            " {} │ {} 项 │ j/k 选择 · Enter 打开 · Esc 上级 · r 刷新 · q 退出",
            trunc_tail(&app.browse_dir.display().to_string(), budget),
            app.browse_entries.len()
        );
    }
    let mut s = format!(
        " {} │ {} 行 │ {:>3}%",
        trunc_tail(&app.title, budget),
        app.doc.line_count(),
        app.progress_percent()
    );
    if let (Some(q), Some(idx)) = (&app.search_query, app.match_idx) {
        s.push_str(&format!(" │ /{q} {}/{}", idx + 1, app.matches.len()));
    } else if let Some(q) = &app.search_query {
        s.push_str(&format!(" │ /{q} ({} 处)", app.matches.len()));
    }
    let hint = match app.mode {
        Mode::Toc => "大纲: j/k 选择 · Enter 跳转 · t 关闭".to_string(),
        Mode::Settings => "设置: j/k 选择 · Enter/←/→ 修改 · s 关闭".to_string(),
        Mode::ZoneInput => {
            format!("安全区列数: {}▏ (0-6, Enter 确认, Esc 取消)", app.zone_input)
        }
        Mode::SearchInput => format!("/{}▏", app.search_input),
        Mode::Read => match &app.status_msg {
            Some(msg) => msg.clone(),
            None if !matches!(app.image_panel, ImagePanel::Closed) => {
                "Esc 关闭图片栏 · Ctrl+左键切换图片 · 鼠标滚轮滚动 · q 退出".to_string()
            }
            None if app.came_from_browse => {
                "Esc 返回目录 · / 搜索 · t 大纲 · Ctrl+O 全部折叠/展开 · q 退出".to_string()
            }
            None => {
                "j/k 滚动 · / 搜索 · t 大纲 · s 设置 · Ctrl+O 全部折叠/展开 · q 退出".to_string()
            }
        },
        _ => unreachable!(),
    };
    s.push_str(&format!(" │ {hint}"));
    s
}

/// 超长标题按显示宽度保留尾部，如 ".../path/xxx.md"。
fn trunc_tail(s: &str, max_w: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_w {
        return s.to_string();
    }
    let mut out = String::from("...");
    let mut w = 3usize;
    for ch in s.chars().rev() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1);
        if w + cw > max_w {
            break;
        }
        out.insert(3, ch);
        w += cw;
    }
    out
}

fn status_bar(app: &App, width: u16) -> Paragraph<'static> {
    Paragraph::new(status_text(app, width))
        .style(Style::new().fg(app.theme.status_fg).bg(app.theme.status_bg))
}

fn draw_toc(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem<'static>> = if app.doc.toc.is_empty() {
        vec![ListItem::new(Line::raw("(无标题)"))]
    } else {
        app.doc
            .toc
            .iter()
            .map(|e| ListItem::new(Line::raw(toc_item_text(e))))
            .collect()
    };
    let selected = if app.doc.toc.is_empty() {
        None
    } else {
        Some(app.toc_selected.min(app.doc.toc.len() - 1))
    };
    let mut state = ListState::default().with_selected(selected);
    let list = List::new(items)
        .block(side_panel_block(" 大纲 ", app.theme))
        .highlight_style(panel_highlight(app.theme))
        .highlight_symbol("▌");
    f.render_stateful_widget(list, area, &mut state);
}

fn toc_item_text(e: &TocEntry) -> String {
    let indent = "  ".repeat((e.level as usize).saturating_sub(1).min(5));
    format!("{indent}{}", e.title)
}

fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    let zone = match app.safe_zone {
        0 => "关".to_string(),
        n => format!("{n} 列"),
    };
    let line_gap = if app.line_gap > 0 { "宽松" } else { "紧凑" };
    let para_gap = match app.para_gap {
        0 => "紧凑",
        1 => "标准",
        _ => "宽松",
    };
    let rows = [
        setting_item("主题", app.theme.name),
        setting_item("安全区", &zone),
        setting_item("行距", line_gap),
        setting_item("段距", para_gap),
        setting_item("折叠默认", if app.fold_default { "收起" } else { "展开" }),
        setting_item("恢复默认", "Enter"),
    ];
    let items: Vec<ListItem<'static>> = rows
        .into_iter()
        .map(|t| ListItem::new(Line::raw(t)))
        .collect();
    let mut state = ListState::default().with_selected(Some(app.settings_sel.min(5)));
    let list = List::new(items)
        .block(side_panel_block(" 设置 (s 关闭) ", app.theme))
        .highlight_style(panel_highlight(app.theme))
        .highlight_symbol("▌");
    f.render_stateful_widget(list, area, &mut state);
}

fn setting_item(label: &str, value: &str) -> String {
    let gap = 10usize.saturating_sub(UnicodeWidthStr::width(label));
    format!("{label}{}{value}", " ".repeat(gap))
}

fn draw_browser(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem<'static>> = if app.browse_entries.is_empty() {
        vec![ListItem::new(Line::raw("(空目录)"))]
    } else {
        app.browse_entries
            .iter()
            .map(|e| ListItem::new(browser_entry_line(e, app.theme)))
            .collect()
    };
    let last = app.browse_entries.len().saturating_sub(1);
    let mut state = ListState::default().with_selected(Some(app.browse_sel.min(last)));
    let list = List::new(items)
        .highlight_style(panel_highlight(app.theme))
        .highlight_symbol("▌");
    f.render_stateful_widget(list, area, &mut state);
}

fn browser_entry_line(e: &BrowserEntry, theme: Theme) -> Line<'static> {
    if e.is_dir {
        let name = if e.name == ".." {
            "▸ ..".to_string()
        } else {
            format!("▸ {}/", e.name)
        };
        Line::from(Span::styled(name, Style::new().fg(theme.accent)))
    } else {
        Line::from(vec![
            Span::raw(e.name.clone()),
            Span::styled(
                format!("  {}", human_size(e.size)),
                Style::new().fg(theme.dim),
            ),
        ])
    }
}

fn side_panel_block<'a>(title: &'a str, theme: crate::theme::Theme) -> Block<'a> {
    Block::new()
        .borders(Borders::LEFT)
        .title(title)
        .border_style(Style::new().fg(theme.border))
        .title_style(Style::new().fg(theme.accent).add_modifier(Modifier::BOLD))
}

fn panel_highlight(theme: crate::theme::Theme) -> Style {
    Style::new()
        .bg(theme.accent)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

/// 把渲染文档序列化为 ANSI 文本（管道/重定向输出用）。
pub fn to_ansi(doc: &Document) -> String {
    let mut out = String::new();
    for line in &doc.lines {
        let start = out.len();
        for span in &line.spans {
            if span.content.is_empty() {
                continue;
            }
            push_sgr(&mut out, span.style);
            out.push_str(span.content.as_ref());
        }
        if out.len() > start {
            out.push_str("\x1b[0m");
        }
        out.push('\n');
    }
    out
}

/// 输出一个片段的 SGR 前缀（先重置再设置，简单可靠）。
fn push_sgr(out: &mut String, style: Style) {
    let mut codes = String::new();
    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push_str(";1");
    }
    if style.add_modifier.contains(Modifier::ITALIC) {
        codes.push_str(";3");
    }
    if style.add_modifier.contains(Modifier::UNDERLINED) {
        codes.push_str(";4");
    }
    if style.add_modifier.contains(Modifier::CROSSED_OUT) {
        codes.push_str(";9");
    }
    if let Some(fg) = style.fg {
        append_color(&mut codes, fg, false);
    }
    if let Some(bg) = style.bg {
        append_color(&mut codes, bg, true);
    }
    if !codes.is_empty() {
        out.push_str(&format!("\x1b[0{}m", codes));
    }
}

fn append_color(codes: &mut String, c: Color, bg: bool) {
    match c {
        Color::Rgb(r, g, b) => {
            codes.push_str(&format!(";{};2;{r};{g};{b}", if bg { 48 } else { 38 }));
        }
        Color::Indexed(i) => {
            codes.push_str(&format!(";{};5;{i}", if bg { 48 } else { 38 }));
        }
        // 默认色：无需任何序列（NO_COLOR 场景保持输出干净）
        Color::Reset => {}
        named => {
            let base = named_sgr(named);
            codes.push_str(&format!(";{}", if bg { base + 10 } else { base }));
        }
    }
}

fn named_sgr(c: Color) -> u8 {
    match c {
        Color::Black => 30,
        Color::Red => 31,
        Color::Green => 32,
        Color::Yellow => 33,
        Color::Blue => 34,
        Color::Magenta => 35,
        Color::Cyan => 36,
        Color::Gray => 37,
        Color::DarkGray => 90,
        Color::LightRed => 91,
        Color::LightGreen => 92,
        Color::LightYellow => 93,
        Color::LightBlue => 94,
        Color::LightMagenta => 95,
        Color::LightCyan => 96,
        Color::White => 97,
        _ => 39,
    }
}

/// 在渲染行上叠加搜索命中高亮（大小写不敏感，保留原有片段样式）。
fn highlight_line<'a>(line: &Line<'a>, query: &str, theme: Theme) -> Line<'a> {
    let q = query.to_ascii_lowercase();
    if q.is_empty() {
        return line.clone();
    }
    let hl_style = Style::new().fg(theme.search_fg).bg(theme.search_bg);
    let mut out: Vec<Span<'a>> = Vec::new();
    for sp in &line.spans {
        let text = sp.content.as_ref();
        let lower = text.to_ascii_lowercase();
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut from = 0usize;
        while let Some(pos) = lower[from..].find(&q) {
            let s = from + pos;
            let e = s + q.len();
            ranges.push((s, e));
            from = e;
        }
        if ranges.is_empty() {
            out.push(sp.clone());
        } else {
            let mut cursor = 0usize;
            for (s, e) in ranges {
                if s > cursor {
                    out.push(Span::styled(text[cursor..s].to_string(), sp.style));
                }
                out.push(Span::styled(text[s..e].to_string(), sp.style.patch(hl_style)));
                cursor = e;
            }
            if cursor < text.len() {
                out.push(Span::styled(text[cursor..].to_string(), sp.style));
            }
        }
    }
    Line::from(out).style(line.style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::theme::DARK;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    fn draw_to_buf(app: &App, w: u16, h: u16) -> Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// TestBackend 会在宽字符后补空格格子，比较前去掉全部空格。
    fn row_range(buf: &Buffer, y: u16, x0: u16, x1: u16) -> String {
        (x0..x1)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    fn page_text(buf: &Buffer, w: u16, rows: u16) -> String {
        (0..rows)
            .map(|y| format!("{}\n", row_range(buf, y, 0, w)))
            .collect()
    }

    #[test]
    fn status_bar_shows_title_progress_and_hints() {
        let app = App::new("内容\n", "test.md".into(), DARK);
        let buf = draw_to_buf(&app, 80, 24);
        let row = row_range(&buf, 23, 0, 80);
        assert!(row.contains("test.md"), "状态栏: {row}");
        assert!(row.contains('%'));
        assert!(row.contains("/搜索"));
        assert!(row.contains("s设置"), "提示应包含设置入口");
    }

    #[test]
    fn status_bar_truncates_long_title() {
        let app = App::new(
            "内容\n",
            "/very/long/path/to/some/deep/directory/readme.md".into(),
            DARK,
        );
        let buf = draw_to_buf(&app, 80, 24);
        let row = row_range(&buf, 23, 0, 80);
        assert!(row.contains("..."), "应有省略号: {row}");
        assert!(row.contains("readme.md"), "应保留文件名: {row}");
        assert!(!row.contains("/very"), "超长路径前缀应被截断: {row}");
    }

    #[test]
    fn trunc_tail_keeps_display_width_budget() {
        assert_eq!(trunc_tail("short.md", 20), "short.md");
        let t = trunc_tail("/极长的中文目录名/再来一层/文件.md", 12);
        assert!(t.starts_with("..."));
        assert!(UnicodeWidthStr::width(t.as_str()) <= 12);
    }

    #[test]
    fn safe_zone_indents_content() {
        let app = App::new("汉字\n", "t.md".into(), DARK);
        assert_eq!(app.safe_zone, 2, "默认 2 列安全区");
        let buf = draw_to_buf(&app, 80, 24);
        assert_eq!(buf[(0, 0)].symbol(), " ");
        assert_eq!(buf[(1, 0)].symbol(), " ");
        assert_eq!(buf[(2, 0)].symbol(), "汉");
        let mut wide = app;
        wide.safe_zone = 4;
        let buf2 = draw_to_buf(&wide, 80, 24);
        assert_eq!(buf2[(3, 0)].symbol(), " ");
        assert_eq!(buf2[(4, 0)].symbol(), "汉");
        wide.safe_zone = 0;
        let buf3 = draw_to_buf(&wide, 80, 24);
        assert_eq!(buf3[(0, 0)].symbol(), "汉");
    }

    #[test]
    fn content_lines_rendered() {
        let app = App::new("第一行\n\n第二行\n", "t.md".into(), DARK);
        let buf = draw_to_buf(&app, 80, 24);
        let page = page_text(&buf, 80, 23);
        assert!(page.contains("第一行"));
        assert!(page.contains("第二行"));
    }

    #[test]
    fn toc_panel_lists_headings_with_selection() {
        let mut app = App::new("# 甲标题\n\n正文\n\n## 乙标题\n", "t.md".into(), DARK);
        app.mode = Mode::Toc;
        let buf = draw_to_buf(&app, 100, 24);
        let right: String = (0..24)
            .map(|y| format!("{}\n", row_range(&buf, y, 70, 100)))
            .collect();
        assert!(right.contains("大纲"), "侧栏: {right}");
        assert!(right.contains("甲标题"));
        assert!(right.contains("乙标题"));
    }

    #[test]
    fn settings_panel_shows_items() {
        let mut app = App::new("内容\n", "t.md".into(), DARK);
        app.mode = Mode::Settings;
        let buf = draw_to_buf(&app, 100, 24);
        let right: String = (0..24)
            .map(|y| format!("{}\n", row_range(&buf, y, 66, 100)))
            .collect();
        assert!(right.contains("设置"), "面板: {right}");
        assert!(right.contains("主题"));
        assert!(right.contains("dark"));
        assert!(right.contains("安全区"));
        assert!(right.contains("2列"), "默认值 2 列: {right}");
        assert!(right.contains("行距"));
        assert!(right.contains("段距"));
        assert!(right.contains("恢复默认"));
        assert!(!right.contains("鼠标捕获"), "鼠标捕获项已移除: {right}");
    }

    #[test]
    fn search_input_visible_in_status() {
        let mut app = App::new("内容\n", "t.md".into(), DARK);
        app.mode = Mode::SearchInput;
        app.search_input = "关键词".into();
        let buf = draw_to_buf(&app, 80, 24);
        assert!(row_range(&buf, 23, 0, 80).contains("/关键词"));
    }

    #[test]
    fn search_hits_highlighted_in_content() {
        let mut app = App::new("目标词在这里\n\n其他内容\n", "t.md".into(), DARK);
        app.search_query = Some("目标".into());
        app.refresh_matches();
        let buf = draw_to_buf(&app, 80, 24);
        let hit_row = (0..23)
            .find(|&y| row_range(&buf, y, 0, 80).contains("目标词"))
            .expect("命中行应可见");
        // 安全区 2 列，“目”在第 2 列且为高亮背景；非命中行保持默认
        assert_eq!(buf[(2, hit_row)].style().bg, Some(DARK.search_bg));
        assert_ne!(buf[(2, 2)].style().bg, Some(DARK.search_bg));
    }

    #[test]
    fn to_ansi_emits_sgr_and_text() {
        let style = Style::new()
            .fg(Color::Rgb(1, 2, 3))
            .add_modifier(Modifier::BOLD);
        let doc = Document {
            lines: vec![Line::from(Span::styled("文本", style))],
            ..Default::default()
        };
        let out = to_ansi(&doc);
        assert!(out.contains("文本"));
        assert!(out.contains("\x1b[0;1;38;2;1;2;3m"), "{out:?}");
        assert!(out.ends_with("\x1b[0m\n"));
    }

    #[test]
    fn to_ansi_plain_line_has_no_color_codes() {
        let doc = Document {
            lines: vec![Line::raw("纯文本".to_string())],
            ..Default::default()
        };
        let out = to_ansi(&doc);
        assert!(out.contains("纯文本"));
        assert!(!out.contains("38;2;"), "无色片段不应带颜色码: {out:?}");
        assert!(!out.contains(';'), "纯文本行不应有 SGR 参数: {out:?}");
    }

    #[test]
    fn highlight_hit_replaces_colors_and_keeps_rest() {        let line = Line::from(vec![Span::styled(
            "abc def".to_string(),
            Style::new().fg(Color::Red),
        )]);
        let out = highlight_line(&line, "def", DARK);
        assert_eq!(out.spans.len(), 2);
        // 非命中部分保留原样式
        assert_eq!(out.spans[0].style.fg, Some(Color::Red));
        assert_eq!(out.spans[0].style.bg, None);
        // 命中部分整体替换为搜索高亮配色
        assert_eq!(out.spans[1].style.fg, Some(DARK.search_fg));
        assert_eq!(out.spans[1].style.bg, Some(DARK.search_bg));
    }

    fn fold_ui_fixture() -> App {
        let src = "开头\n\n<details>\n<summary>点我展开</summary>\n\n隐藏甲\n\n隐藏乙\n\n</details>\n\n结尾可见\n";
        App::new(src, "f.md".into(), DARK)
    }

    #[test]
    fn fold_hides_content_and_shows_summary_line() {
        let mut app = fold_ui_fixture();
        assert_eq!(app.doc.collapsibles.len(), 1);
        app.scroll = app.doc.collapsibles[0].line;
        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(app.folds[0]);
        let joined: String = app
            .doc
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect();
        assert!(joined.contains("隐藏甲"), "原文档不变");
        assert!(joined.contains("点我展开"), "summary 不应被吞");
        let buf = draw_to_buf(&app, 80, 24);
        let page = page_text(&buf, 80, 23);
        assert!(!page.contains("隐藏甲"), "折叠后内容应隐藏: {page}");
        assert!(page.contains("▸点我展开"), "应出现 summary 行: {page}");
        assert!(page.contains("结尾可见"), "折叠区之后的内容仍可见: {page}");
    }

    #[test]
    fn fold_view_fills_viewport_no_black_gap() {
        // 折叠大块后，显示行数必须仍填满视口（后续内容上移补位，不出现黑条）
        let src = "<details>\n<summary>块</summary>\n\n被隐藏\n\n</details>\n\n可见一\n\n可见二\n\n可见三\n";
        let mut app = App::new(src, "g.md".into(), DARK);
        app.view_height = 6;
        app.folds[0] = true;
        app.scroll = app.doc.collapsibles[0].line;
        let out = apply_folds(
            &app.doc.lines,
            &app.doc.collapsibles,
            &app.folds,
            app.scroll,
            6,
            DARK,
        );
        assert_eq!(out.len(), 6, "必须填满视口");
        let text = |l: &Line<'static>| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert!(out.iter().any(|l| text(l).contains("可见一")));
        assert!(out.iter().any(|l| text(l).contains("可见三")));
    }

    #[test]
    fn fold_scroll_inside_hidden_maps_to_summary() {
        let mut app = fold_ui_fixture();
        app.folds[0] = true;
        let start = app.doc.collapsibles[0].line;
        let out = apply_folds(
            &app.doc.lines,
            &app.doc.collapsibles,
            &app.folds,
            start + 1,
            12,
            DARK,
        );
        let first: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first.contains("点我展开"), "落在隐藏区间内应从 summary 开始: {first}");
    }

    #[test]
    fn fold_expanded_indents_interior_lines() {
        let app = fold_ui_fixture();
        let out = apply_folds(
            &app.doc.lines,
            &app.doc.collapsibles,
            &app.folds,
            0,
            12,
            DARK,
        );
        let text = |l: &Line<'static>| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        // 块外行不缩进
        assert!(text(&out[0]).contains("开头") && !text(&out[0]).starts_with("  "));
        // summary 行不缩进（块头），块内行缩进 2 格
        let summary_idx = out
            .iter()
            .position(|l| text(l).contains("点我展开"))
            .unwrap();
        assert!(!text(&out[summary_idx]).starts_with("  "));
        assert!(
            out.iter()
                .any(|l| text(l).contains("隐藏甲") && text(l).starts_with("  ")),
            "块内行应整段缩进: {:?}",
            out.iter().map(text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn image_hit_test_uses_visible_rows_and_ignores_whitespace() {
        let mut app = App::new("![图](assets/a.png) 普通正文\n", "t.md".into(), DARK);
        let hit = app.doc.image_hits[0].clone();
        let area = layout_for(
            Rect::new(0, 0, app.render_width, app.view_height as u16 + 1),
            &app,
        )
        .document;
        assert_eq!(image_hit_at(&app, area.x + hit.start_col as u16, hit.line as u16), Some(hit.clone()));
        assert!(image_hit_at(&app, area.x + hit.end_col as u16 + 1, hit.line as u16).is_none());

        let src = "<details>\n<summary>块</summary>\n\n![隐藏](hidden.png)\n\n</details>\n可见\n";
        app = App::new(src, "fold.md".into(), DARK);
        let hidden = app.doc.image_hits[0].clone();
        app.folds[0] = true;
        for y in 0..app.view_height as u16 {
            assert!(image_hit_at(&app, 2, y).is_none(), "折叠内容不可命中: y={y}");
        }
        app.folds[0] = false;
        let rows = visible_rows(
            &app.doc.lines,
            &app.doc.collapsibles,
            &app.folds,
            app.scroll,
            app.view_height,
            app.theme,
        );
        let row = rows
            .iter()
            .position(|row| row.source_line == Some(hidden.line))
            .expect("展开后图片行可见");
        let x = area.x + hidden.start_col as u16 + rows[row].extra_indent as u16;
        assert_eq!(image_hit_at(&app, x, row as u16), Some(hidden));
    }

    #[test]
    fn path_only_image_panel_draws_reference_without_loading_image() {
        let mut app = App::new("![图](assets/a.png)\n", "t.md".into(), DARK);
        app.image_panel = ImagePanel::PathOnly {
            image: app.doc.images[0].clone(),
        };
        let buf = draw_to_buf(&app, 100, 24);
        let right: String = (0..24)
            .map(|y| format!("{}\n", row_range(&buf, y, 64, 100)))
            .collect();
        assert!(right.contains("PathOnly"), "图片面板: {right}");
        assert!(right.contains("assets/a.png"));
        assert!(right.contains("原生图片支持未启用"));
    }

    #[test]
    fn zone_input_hint_shown_in_status() {
        let mut app = App::new("内容\n", "t.md".into(), DARK);
        app.mode = Mode::ZoneInput;
        app.zone_input = "4".into();
        let buf = draw_to_buf(&app, 80, 24);
        let row = row_range(&buf, 23, 0, 80);
        assert!(row.contains("安全区列数:4"), "状态栏: {row}");
        assert!(row.contains("Enter确认"));
    }
}


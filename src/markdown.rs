use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use pulldown_cmark::{
    Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::highlight::Highlighter;
use crate::theme::Theme;

/// 大纲行号占位：所在块尚未渲染。
pub const UNRESOLVED_LINE: usize = usize::MAX;

/// 大纲条目：一个标题及其在渲染后行流中的位置。
#[derive(Debug, Clone)]
pub struct TocEntry {
    pub level: u8,
    pub title: String,
    pub line: usize,
    /// 所在源文块索引（懒渲染下用于按需渲染定位）
    pub chunk: usize,
}

/// 可折叠块（对应 `<details>/<summary>`）：summary 行到块结束的渲染行区间。
#[derive(Debug, Clone)]
pub struct Collapsible {
    /// summary 行在 doc.lines 中的索引
    pub line: usize,
    /// 块结束（不含）：隐藏区间为 (line, end)
    pub end: usize,
    pub title: String,
}

/// 文档中的一个图片引用。`raw_path` 保留 Markdown 原文，路径解析只在有
/// Markdown 文件路径时进行；渲染器不会读取图片内容。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageRef {
    pub id: usize,
    pub raw_path: String,
    pub resolved_path: Option<PathBuf>,
    pub is_local: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageHitPart {
    Alt,
    Path,
}

/// 图片显示文字在最终折行行流中的命中范围，列数使用终端 display width。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageHit {
    pub image: usize,
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub part: ImageHitPart,
}

/// 渲染中的文档：已渲染的行流 + 大纲 + 图片命中元数据 + 待渲染的源文块。
#[derive(Debug, Clone, Default)]
pub struct Document {
    pub lines: Vec<Line<'static>>,
    pub toc: Vec<TocEntry>,
    pub collapsibles: Vec<Collapsible>,
    pub images: Vec<ImageRef>,
    pub image_hits: Vec<ImageHit>,
    /// 懒渲染：尚未渲染的源文块队列
    pub pending: VecDeque<String>,
    pub rendered_chunks: usize,
    /// 源文总行数与已渲染源文行数（懒渲染进度估算用）
    pub total_source_lines: usize,
    pub rendered_source_lines: usize,
}

impl Document {
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn fully_rendered(&self) -> bool {
        self.pending.is_empty()
    }

    /// 按 Markdown 文件所在目录解析本地图片路径；远程 URI 永远不解析。
    pub fn resolve_image_paths(&mut self, source_path: Option<&Path>) {
        let base = source_path.and_then(Path::parent);
        for image in &mut self.images {
            image.resolved_path = if image.is_local {
                resolve_local_path(&image.raw_path, base)
            } else {
                None
            };
        }
    }
}

/// 渲染版式参数（安全区与行/段距）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderStyle {
    pub safe_zone: u8,
    /// 行距：相邻正文行之间插入的空行数（0=紧凑 1=宽松）；
    /// 代码块行、表格行、分割线等"排除行"不参与
    pub line_gap: u8,
    /// 段距：块之间的空行数（0=紧凑 1=标准 2=宽松）
    pub para_gap: u8,
}

impl Default for RenderStyle {
    fn default() -> Self {
        Self {
            safe_zone: 1,
            line_gap: 0,
            para_gap: 1,
        }
    }
}

/// 渲染器：持有 syntect 高亮资源（加载较重，跨多次渲染复用）。
pub struct Renderer {
    hl: Highlighter,
    theme: Theme,
}

impl Renderer {
    pub fn new(theme: Theme) -> Self {
        Self {
            hl: Highlighter::new(theme.code_theme, theme.code_lift),
            theme,
        }
    }

    /// 将 Markdown 源文渲染为指定宽度下的样式化行流。
    /// 行内折行按显示宽度计算（CJK 占 2 列），续行对齐列表/引用前缀。
    /// safe_zone 为内容安全区宽度（列），代码块据此留出四周底色安全区。
    /// 超过 LAZY_THRESHOLD 的大文件分块懒渲染：先渲染首块保证秒开。
    pub fn render_with(&self, source: &str, width: u16, style: RenderStyle) -> Document {
        let mut doc = Document::default();
        if source.len() > LAZY_THRESHOLD {
            let chunks = split_chunks(source);
            doc.total_source_lines = chunks.iter().map(|c| c.lines().count()).sum();
            doc.toc = scan_headings(&chunks)
                .into_iter()
                .map(|(chunk, level, title)| TocEntry {
                    level,
                    title,
                    line: UNRESOLVED_LINE,
                    chunk,
                })
                .collect();
            doc.pending = chunks.into();
            self.render_next_chunk(&mut doc, width, style);
            return doc;
        }
        doc.toc = self.render_source(&mut doc, source, width, style);
        doc
    }

    /// 渲染下一懒加载块；无剩余块时返回 false。
    pub fn render_next_chunk(&self, doc: &mut Document, width: u16, style: RenderStyle) -> bool {
        let Some(chunk_src) = doc.pending.pop_front() else {
            return false;
        };
        let idx = doc.rendered_chunks;
        let src_lines = chunk_src.lines().count();
        let chunk_toc = self.render_source(doc, &chunk_src, width, style);
        doc.rendered_chunks += 1;
        doc.rendered_source_lines += src_lines;
        // 把本块实际渲染出的标题，回填到预扫描的大纲上
        let mut rendered = chunk_toc.into_iter();
        for entry in doc
            .toc
            .iter_mut()
            .filter(|e| e.chunk == idx && e.line == UNRESOLVED_LINE)
        {
            match rendered.next() {
                Some(r) => {
                    entry.line = r.line;
                    entry.title = r.title;
                }
                None => break,
            }
        }
        true
    }

    /// 渲染一段源文到 doc（事件循环主体），返回本块产生的大纲条目。
    fn render_source(
        &self,
        doc: &mut Document,
        source: &str,
        width: u16,
        style: RenderStyle,
    ) -> Vec<TocEntry> {
        let width = (width as usize).max(20);
        let start = doc.lines.len();
        let mut ctx = Ctx {
            doc,
            width,
            theme: self.theme,
            quote_depth: 0,
            indent_stack: Vec::new(),
            bullet: None,
            style,
            toc_out: Vec::new(),
            excluded: Vec::new(),
            details_stack: Vec::new(),
            coll_out: Vec::new(),
        };

        let opts = Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
        let mut buf: Vec<MarkedSpan> = Vec::new();
        let mut style_stack: Vec<Style> = Vec::new();
        let mut cur_style = Style::new();
        let mut heading_level: Option<u8> = None;
        // (链接目标, 进入链接时 buf 的长度)，用于在链接尾部展示 URL
        let mut link_stack: Vec<(String, usize)> = Vec::new();
        // 图片与链接分开维护：图片的 alt/path 命中信息不能被链接 URL 逻辑吞掉。
        let mut image_stack: Vec<ImageState> = Vec::new();
        let mut list_stack: Vec<ListState> = Vec::new();
        let mut code: Option<(Option<String>, String)> = None;
        let mut table: Option<TableState> = None;

        for event in Parser::new_ext(source, opts) {
            // 代码块内容：原样收集，交给 syntect
            if let Some((_, cbuf)) = code.as_mut()
                && let Event::Text(t) = &event
            {
                cbuf.push_str(t);
                continue;
            }
            // 表格单元格：保留图片的 alt/path 文字和图片引用元数据，
            // 其他复杂内联结构仍按表格的纯文本路径收集。
            if let Some(ts) = table.as_mut()
                && ts.cell.is_some()
            {
                match &event {
                    Event::Start(Tag::Image { dest_url, .. }) => {
                        let raw_path = dest_url.to_string();
                        let image = ctx.register_image(raw_path.clone());
                        ts.image_stack.push(TableImage {
                            image,
                            raw_path,
                            alt: String::new(),
                        });
                        continue;
                    }
                    Event::Text(t) | Event::Code(t) => {
                        if let Some(image) = ts.image_stack.last_mut() {
                            image.alt.push_str(t);
                        } else {
                            ts.cell.as_mut().unwrap().push_str(t);
                        }
                        continue;
                    }
                    Event::SoftBreak | Event::HardBreak => {
                        if let Some(image) = ts.image_stack.last_mut() {
                            image.alt.push(' ');
                        } else {
                            ts.cell.as_mut().unwrap().push(' ');
                        }
                        continue;
                    }
                    Event::End(TagEnd::Image) => {
                        if let Some(image) = ts.image_stack.pop() {
                            let alt = expand_tabs(&image.alt).into_owned();
                            let cell_start = display_width(ts.cell.as_deref().unwrap_or(""));
                            let alt_width = display_width(&alt);
                            if alt_width > 0 {
                                ts.cell_images.push(TableImageHit {
                                    image: image.image,
                                    start_col: cell_start,
                                    end_col: cell_start + alt_width,
                                    part: ImageHitPart::Alt,
                                });
                            }
                            if image.raw_path.is_empty() {
                                ts.cell.as_mut().unwrap().push_str(&alt);
                            } else {
                                let path_start = cell_start + alt_width + 2;
                                ts.cell_images.push(TableImageHit {
                                    image: image.image,
                                    start_col: path_start,
                                    end_col: path_start + display_width(&image.raw_path),
                                    part: ImageHitPart::Path,
                                });
                                ts.cell
                                    .as_mut()
                                    .unwrap()
                                    .push_str(&format!("{} ({})", alt, image.raw_path));
                            }
                        }
                        continue;
                    }
                    Event::End(TagEnd::TableCell) => {
                        let c = ts.cell.take().unwrap_or_default();
                        ts.cur_row.push(expand_tabs(&c).into_owned());
                        ts.cur_row_images.push(std::mem::take(&mut ts.cell_images));
                        continue;
                    }
                    _ => {}
                }
            }

            match event {
                Event::Start(tag) => match tag {
                    Tag::Paragraph => {}
                    Tag::Heading { level, .. } => {
                        let lvl = match level {
                            HeadingLevel::H1 => 1,
                            HeadingLevel::H2 => 2,
                            HeadingLevel::H3 => 3,
                            HeadingLevel::H4 => 4,
                            HeadingLevel::H5 => 5,
                            HeadingLevel::H6 => 6,
                        };
                        heading_level = Some(lvl);
                        style_stack.push(cur_style);
                        cur_style = heading_style(&self.theme, lvl);
                    }
                    Tag::BlockQuote(_) => ctx.quote_depth += 1,
                    Tag::CodeBlock(kind) => {
                        let lang = match kind {
                            CodeBlockKind::Fenced(info) => info
                                .split([',', ' '])
                                .next()
                                .filter(|s| !s.is_empty())
                                .map(String::from),
                            CodeBlockKind::Indented => None,
                        };
                        code = Some((lang, String::new()));
                    }
                    Tag::List(start) => {
                        // item 内容直接是嵌套列表：先落下已收集的项文本，再进嵌套
                        if ctx.bullet.is_some() {
                            let pending = std::mem::take(&mut buf);
                            ctx.emit(pending);
                        }
                        list_stack.push(ListState {
                            ordered: start.is_some(),
                            next: start.unwrap_or(1),
                        });
                    }
                    Tag::Item => {
                        let marker = match list_stack.last_mut() {
                            Some(st) if st.ordered => {
                                let m = format!("{}. ", st.next);
                                st.next += 1;
                                m
                            }
                            _ => "• ".to_string(),
                        };
                        ctx.indent_stack.push(display_width(&marker));
                        ctx.bullet = Some(Span::styled(marker, Style::new().fg(self.theme.accent)));
                    }
                    Tag::Table(aligns) => {
                        table = Some(TableState {
                            aligns,
                            header: None,
                            header_images: None,
                            rows: Vec::new(),
                            cur_row: Vec::new(),
                            cur_row_images: Vec::new(),
                            cell: None,
                            cell_images: Vec::new(),
                            image_stack: Vec::new(),
                            in_head: false,
                        });
                    }
                    Tag::TableHead => {
                        if let Some(t) = table.as_mut() {
                            t.in_head = true;
                        }
                    }
                    Tag::TableRow => {
                        if let Some(t) = table.as_mut() {
                            t.cur_row = Vec::new();
                            t.cur_row_images = Vec::new();
                        }
                    }
                    Tag::TableCell => {
                        if let Some(t) = table.as_mut() {
                            t.cell = Some(String::new());
                            t.cell_images = Vec::new();
                        }
                    }
                    Tag::Emphasis => {
                        style_stack.push(cur_style);
                        cur_style = cur_style.add_modifier(Modifier::ITALIC);
                    }
                    Tag::Strong => {
                        style_stack.push(cur_style);
                        cur_style = cur_style.add_modifier(Modifier::BOLD);
                    }
                    Tag::Strikethrough => {
                        style_stack.push(cur_style);
                        cur_style = cur_style.add_modifier(Modifier::CROSSED_OUT);
                    }
                    Tag::Link { dest_url, .. } => {
                        link_stack.push((dest_url.to_string(), buf.len()));
                        style_stack.push(cur_style);
                        cur_style = Style::new()
                            .fg(self.theme.accent)
                            .add_modifier(Modifier::UNDERLINED);
                    }
                    Tag::Image { dest_url, .. } => {
                        let raw_path = dest_url.to_string();
                        let image = ctx.register_image(raw_path.clone());
                        image_stack.push(ImageState { image, raw_path });
                        style_stack.push(cur_style);
                        cur_style = Style::new()
                            .fg(self.theme.dim)
                            .add_modifier(Modifier::ITALIC);
                    }
                    _ => {}
                },
                Event::End(end) => match end {
                    TagEnd::Paragraph => {
                        ctx.emit(std::mem::take(&mut buf));
                        ctx.blank();
                    }
                    TagEnd::Heading(_) => {
                        let level = heading_level.take().unwrap_or(1);
                        let title: String = buf.iter().map(|s| s.span.content.as_ref()).collect();
                        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
                        ctx.toc_out.push(TocEntry {
                            level,
                            title,
                            line: ctx.doc.lines.len(),
                            chunk: 0,
                        });
                        let mut spans = std::mem::take(&mut buf);
                        spans.insert(
                            0,
                            MarkedSpan::plain(heading_prefix(&self.theme, level)),
                        );
                        ctx.emit(spans);
                        ctx.blank();
                        cur_style = style_stack.pop().unwrap_or_default();
                    }
                    TagEnd::BlockQuote(_) => ctx.quote_depth = ctx.quote_depth.saturating_sub(1),
                    TagEnd::CodeBlock => {
                        if let Some((lang, cbuf)) = code.take() {
                            let lines = self.hl.highlight(&expand_tabs(&cbuf), lang.as_deref());
                            if !lines.is_empty() {
                                let bg = self.hl.background().unwrap_or(self.theme.code_bg);
                                let bg_style = Style::new().bg(bg);
                                let budget = ctx.content_budget();
                                let inset = ctx.style.safe_zone as usize;
                                // 上下安全区：整行底色空行；已知语言时顶部行内嵌语言标签
                                let top = match lang.as_deref().filter(|l| !l.is_empty()) {
                                    Some(l) => {
                                        let tag = format!(" {l} ");
                                        let rest =
                                            budget.saturating_sub(inset + display_width(&tag));
                                        let mut v: Vec<Span<'static>> = Vec::new();
                                        if inset > 0 {
                                            v.push(Span::styled(" ".repeat(inset), bg_style));
                                        }
                                        v.push(Span::styled(
                                            tag,
                                            Style::new().fg(ctx.theme.dim).bg(bg),
                                        ));
                                        v.push(Span::styled(" ".repeat(rest), bg_style));
                                        v
                                    }
                                    None => vec![Span::styled(" ".repeat(budget), bg_style)],
                                };
                                ctx.raw_line(top);
                                for lspans in lines {
                                    let mut spans: Vec<Span<'static>> = Vec::new();
                                    // 左侧安全区
                                    if inset > 0 {
                                        spans.push(Span::styled(" ".repeat(inset), bg_style));
                                    }
                                    let mut cw = inset;
                                    for (st, s) in lspans {
                                        cw += display_width(&s);
                                        // 字符处与空白处统一底色
                                        spans.push(Span::styled(s, st.bg(bg)));
                                    }
                                    if cw < budget {
                                        spans.push(Span::styled(" ".repeat(budget - cw), bg_style));
                                    }
                                    ctx.raw_line(spans);
                                }
                                ctx.raw_line(vec![Span::styled(" ".repeat(budget), bg_style)]);
                                ctx.blank();
                            }
                        }
                    }
                    TagEnd::Item => {
                        // 紧凑列表项没有 Paragraph 包裹，在项结束时冲刷
                        ctx.emit(std::mem::take(&mut buf));
                        ctx.indent_stack.pop();
                    }
                    TagEnd::List(_) => {
                        list_stack.pop();
                    }
                    TagEnd::Table => {
                        if let Some(t) = table.take() {
                            render_table(&mut ctx, &t);
                            ctx.blank();
                        }
                    }
                    TagEnd::TableHead => {
                        if let Some(t) = table.as_mut() {
                            t.header = Some(std::mem::take(&mut t.cur_row));
                            t.header_images = Some(std::mem::take(&mut t.cur_row_images));
                            t.in_head = false;
                        }
                    }
                    TagEnd::TableRow => {
                        if let Some(t) = table.as_mut()
                            && !t.in_head
                        {
                            t.rows.push((
                                std::mem::take(&mut t.cur_row),
                                std::mem::take(&mut t.cur_row_images),
                            ));
                        }
                    }
                    TagEnd::Link => {
                        cur_style = style_stack.pop().unwrap_or_default();
                        if let Some((url, mark)) = link_stack.pop() {
                            let mark = mark.min(buf.len());
                            let plain: String = buf[mark..].iter().map(|s| s.span.content.as_ref()).collect();
                            if !url.is_empty() && url != plain {
                                buf.push(MarkedSpan::plain(Span::styled(
                                    format!(" ({url})"),
                                    Style::new().fg(self.theme.dim),
                                )));
                            }
                        }
                    }
                    TagEnd::Image => {
                        cur_style = style_stack.pop().unwrap_or_default();
                        if let Some(state) = image_stack.pop()
                            && !state.raw_path.is_empty()
                        {
                            let dim = Style::new().fg(self.theme.dim);
                            buf.push(MarkedSpan::plain(Span::styled(" (", dim)));
                            buf.push(MarkedSpan::marked(
                                Span::styled(state.raw_path.clone(), dim),
                                state.image,
                                ImageHitPart::Path,
                            ));
                            buf.push(MarkedSpan::plain(Span::styled(")", dim)));
                        }
                    }
                    TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                        cur_style = style_stack.pop().unwrap_or_default();
                    }
                    _ => {}
                },
                Event::Text(t) => buf.push(MarkedSpan::marked_if_image(
                    Span::styled(expand_tabs(&t).into_owned(), cur_style),
                    image_stack.last().map(|state| state.image),
                    ImageHitPart::Alt,
                )),
                Event::Code(t) => buf.push(MarkedSpan::marked_if_image(
                    Span::styled(
                        expand_tabs(&t).into_owned(),
                        Style::new().fg(self.theme.code_fg).bg(self.theme.code_bg),
                    ),
                    image_stack.last().map(|state| state.image),
                    ImageHitPart::Alt,
                )),
                Event::SoftBreak => buf.push(MarkedSpan::marked_if_image(
                    Span::raw(" "),
                    image_stack.last().map(|state| state.image),
                    ImageHitPart::Alt,
                )),
                Event::HardBreak => ctx.emit(std::mem::take(&mut buf)),
                Event::Rule => {
                    let w = ctx.content_budget();
                    ctx.raw_line(vec![Span::styled(
                        "─".repeat(w),
                        Style::new().fg(self.theme.border),
                    )]);
                }
                Event::TaskListMarker(checked) => {
                    if let Some(b) = ctx.bullet.take() {
                        let mut s = b.content.to_string();
                        s.push_str(if checked { "[x] " } else { "[ ] " });
                        ctx.bullet = Some(Span::styled(s, b.style));
                    }
                }
                Event::Html(h) => {
                    self.handle_html(&mut ctx, &h);
                }
                Event::InlineHtml(h) if !self.handle_html(&mut ctx, &h) => {
                    buf.push(MarkedSpan::plain(Span::styled(
                        h.to_string(),
                        Style::new().fg(self.theme.dim),
                    )));
                }
                _ => {}
            }
        }
        if !buf.is_empty() {
            ctx.emit(std::mem::take(&mut buf));
        }
        // 行距后处理：在相邻正文行之间插入空行（排除行不参与）
        let map = apply_line_gap(ctx.doc, start, &ctx.excluded, ctx.style.line_gap);
        // 插行后把本块大纲和图片命中行号映射到新位置。
        for hit in ctx.doc.image_hits.iter_mut().filter(|h| h.line >= start) {
            if let Some(&n) = map.get(hit.line - start) {
                hit.line = n;
            }
        }
        for e in &mut ctx.toc_out {
            if e.line != UNRESOLVED_LINE && e.line >= start
                && let Some(&n) = map.get(e.line - start)
            {
                e.line = n;
            }
        }
        // 折叠块行号同样重定位，然后并入文档
        for c in &mut ctx.coll_out {
            if c.line >= start
                && let Some(&n) = map.get(c.line - start)
            {
                c.line = n;
            }
            if c.end >= start {
                c.end = map
                    .get(c.end - start)
                    .copied()
                    .unwrap_or(ctx.doc.lines.len());
            }
        }
        ctx.doc.collapsibles.append(&mut ctx.coll_out);
        std::mem::take(&mut ctx.toc_out)
    }

    /// 逐行处理 HTML 事件中的 details/summary 结构；返回 true 表示已消费。
    /// pulldown 可能把 `<details>`、`<summary>` 与紧随的内容行合进同一个
    /// HTML 块事件，必须逐行处理，否则内容行会被整体吞掉。
    fn handle_html(&self, ctx: &mut Ctx<'_>, raw: &str) -> bool {
        let mut handled = false;
        for line in raw.lines() {
            let lower = line.to_lowercase();
            if lower.contains("</details") {
                if let Some((line_no, title)) = ctx.details_stack.pop()
                    && line_no != usize::MAX
                {
                    ctx.coll_out.push(Collapsible {
                        line: line_no,
                        end: ctx.doc.lines.len(),
                        title,
                    });
                }
                handled = true;
            } else if lower.contains("<details") {
                ctx.details_stack.push((usize::MAX, String::new()));
                handled = true;
            } else if lower.contains("<summary") {
                let title = extract_summary(line);
                let start_line = ctx.doc.lines.len();
                ctx.emit(vec![
                    MarkedSpan::plain(Span::styled("▸ ", Style::new().fg(ctx.theme.accent))),
                    MarkedSpan::plain(Span::styled(
                        title.clone(),
                        Style::new()
                            .fg(ctx.theme.accent)
                            .add_modifier(Modifier::BOLD),
                    )),
                ]);
                if let Some(entry) = ctx.details_stack.last_mut() {
                    entry.0 = start_line;
                    entry.1 = title;
                }
                handled = true;
            } else if !ctx.details_stack.is_empty() {
                // details 块内、未被 Markdown 解析的内容行：按纯文本输出，避免吞行
                let text = strip_tags(line).trim().to_string();
                if !text.is_empty() {
                    ctx.emit(vec![MarkedSpan::plain(Span::raw(text))]);
                }
                handled = true;
            }
        }
        handled
    }
}

/// 大文件懒渲染阈值（字节）。
pub const LAZY_THRESHOLD: usize = 1024 * 1024;
/// 单个懒渲染块的目标源文行数。
const CHUNK_LINES: usize = 256;

/// 把源文按块边界（空行）切成 >=CHUNK_LINES 行的块；
/// 空行处切断保证大多数块内结构（段落/表格/围栏）不被劈开。
fn split_chunks(source: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut count = 0usize;
    for line in source.lines() {
        cur.push_str(line);
        cur.push('\n');
        count += 1;
        if count >= CHUNK_LINES && line.trim().is_empty() {
            chunks.push(std::mem::take(&mut cur));
            count = 0;
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// 快速扫描全部 ATX 标题（不完整解析），返回 (块索引, 级别, 标题)。
/// 跳过围栏代码块内的 `#` 行；setext 标题不识别（大文件模式的可接受近似）。
fn scan_headings(chunks: &[String]) -> Vec<(usize, u8, String)> {
    let mut out = Vec::new();
    for (ci, chunk) in chunks.iter().enumerate() {
        let mut in_fence = false;
        for line in chunk.lines() {
            let t = line.trim_start();
            if t.starts_with("```") || t.starts_with("~~~") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }
            let indent = line.len() - t.len();
            if indent > 3 {
                continue;
            }
            let hashes = t.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&hashes) {
                let rest = &t[hashes..];
                if rest.is_empty() || rest.starts_with(' ') {
                    let title = rest.trim().to_string();
                    if !title.is_empty() {
                        out.push((ci, hashes as u8, title));
                    }
                }
            }
        }
    }
    out
}

struct ListState {
    ordered: bool,
    next: u64,
}

struct TableState {
    aligns: Vec<Alignment>,
    header: Option<Vec<String>>,
    header_images: Option<Vec<Vec<TableImageHit>>>,
    rows: Vec<(Vec<String>, Vec<Vec<TableImageHit>>)>,
    cur_row: Vec<String>,
    cur_row_images: Vec<Vec<TableImageHit>>,
    cell: Option<String>,
    cell_images: Vec<TableImageHit>,
    image_stack: Vec<TableImage>,
    in_head: bool,
}

#[derive(Clone)]
struct TableImage {
    image: usize,
    raw_path: String,
    alt: String,
}

#[derive(Clone)]
struct TableImageHit {
    image: usize,
    start_col: usize,
    end_col: usize,
    part: ImageHitPart,
}

struct TableRenderRow<'a> {
    cells: &'a Vec<String>,
    images: &'a [Vec<TableImageHit>],
    is_head: bool,
}

#[derive(Clone)]
struct MarkedSpan {
    span: Span<'static>,
    image: Option<ImageMarker>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ImageMarker {
    image: usize,
    part: ImageHitPart,
}

struct ImageState {
    image: usize,
    raw_path: String,
}

impl MarkedSpan {
    fn plain(span: Span<'static>) -> Self {
        Self { span, image: None }
    }

    fn marked(span: Span<'static>, image: usize, part: ImageHitPart) -> Self {
        Self {
            span,
            image: Some(ImageMarker { image, part }),
        }
    }

    fn marked_if_image(
        span: Span<'static>,
        image: Option<usize>,
        part: ImageHitPart,
    ) -> Self {
        match image {
            Some(id) => Self::marked(span, id, part),
            None => Self::plain(span),
        }
    }
}

/// 一次渲染的输出上下文：持有行流与列表/引用前缀状态。
struct Ctx<'a> {
    doc: &'a mut Document,
    width: usize,
    theme: Theme,
    quote_depth: usize,
    /// 各层列表项的标记宽度栈；content 起点为 quote + 前几层宽 + 当前层标记
    indent_stack: Vec<usize>,
    /// 待挂在下一逻辑行行首的列表标记（如 "• " / "1. " / "1. [x] "）
    bullet: Option<Span<'static>>,
    /// 版式：安全区与行/段距
    style: RenderStyle,
    /// 本次渲染产生的大纲条目
    toc_out: Vec<TocEntry>,
    /// 排除行（代码块行、表格行、分割线）在 doc.lines 中的绝对索引
    excluded: Vec<usize>,
    /// `<details>` 嵌套栈：(summary 行索引, 标题)；行索引 usize::MAX 表示未见 summary
    details_stack: Vec<(usize, String)>,
    /// 本次渲染完成的折叠块
    coll_out: Vec<Collapsible>,
}

impl<'a> Ctx<'a> {
    fn quote_w(&self) -> usize {
        self.quote_depth * 2
    }

    fn pre_w(&self) -> usize {
        self.indent_stack
            .iter()
            .rev()
            .skip(1)
            .copied()
            .sum::<usize>()
    }

    fn last_w(&self) -> usize {
        self.indent_stack.last().copied().unwrap_or(0)
    }

    fn content_budget(&self) -> usize {
        self.width
            .saturating_sub(self.quote_w() + self.pre_w() + self.last_w())
            .max(10)
    }

    fn quote_prefix(&self) -> Vec<Span<'static>> {
        if self.quote_depth == 0 {
            Vec::new()
        } else {
            vec![Span::styled(
                "│ ".repeat(self.quote_depth),
                Style::new().fg(self.theme.quote),
            )]
        }
    }

    fn register_image(&mut self, raw_path: String) -> usize {
        let id = self.doc.images.len();
        self.doc.images.push(ImageRef {
            id,
            is_local: is_local_image_path(&raw_path),
            raw_path,
            resolved_path: None,
        });
        id
    }

    /// 折行输出一个逻辑行；首行消耗列表标记，续行按前缀宽度对齐。
    fn emit(&mut self, spans: Vec<MarkedSpan>) {
        if spans.is_empty() && self.bullet.is_none() {
            return;
        }
        let budget = self.content_budget();
        let wrapped = wrap_marked_spans(&spans, budget);
        let quote = self.quote_prefix();
        let indent = Span::raw(" ".repeat(self.pre_w()));
        let cont = Span::raw(" ".repeat(self.pre_w() + self.last_w()));
        let bullet = self.bullet.take();
        let last_w = self.last_w();
        let has_indent = !self.indent_stack.is_empty();
        for (i, seg) in wrapped.into_iter().enumerate() {
            let mut line: Vec<Span<'static>> = Vec::new();
            line.extend(quote.iter().cloned());
            if i == 0 {
                line.push(indent.clone());
                if has_indent {
                    match &bullet {
                        Some(b) => line.push(b.clone()),
                        None => line.push(Span::raw(" ".repeat(last_w))),
                    }
                }
            } else {
                line.push(cont.clone());
            }
            let mut col: usize = line.iter().map(|span| display_width(&span.content)).sum();
            let line_no = self.doc.lines.len();
            for marked in seg {
                let width = display_width(&marked.span.content);
                if let Some(marker) = marked.image
                    && width > 0
                {
                    let same = self.doc.image_hits.last().is_some_and(|hit| {
                        hit.image == marker.image
                            && hit.part == marker.part
                            && hit.line == line_no
                            && hit.end_col == col
                    });
                    if same {
                        self.doc.image_hits.last_mut().unwrap().end_col += width;
                    } else {
                        self.doc.image_hits.push(ImageHit {
                            image: marker.image,
                            line: line_no,
                            start_col: col,
                            end_col: col + width,
                            part: marker.part,
                        });
                    }
                }
                col += width;
                line.push(marked.span);
            }
            self.doc.lines.push(Line::from(line));
        }
    }

    /// 输出不折行的原始行（表格、分割线、代码行），仍带前缀与标记。
    fn raw_line(&mut self, spans: Vec<Span<'static>>) {
        self.raw_line_with_hits(spans, &[]);
    }

    fn raw_line_with_hits(&mut self, mut spans: Vec<Span<'static>>, hits: &[TableImageHit]) {
        let line_no = self.doc.lines.len();
        self.excluded.push(line_no);
        let quote = self.quote_prefix();
        let indent = Span::raw(" ".repeat(self.pre_w() + self.last_w()));
        let bullet = self.bullet.take();
        let last_w = self.last_w();
        let has_indent = !self.indent_stack.is_empty();
        let mut line: Vec<Span<'static>> = Vec::new();
        line.extend(quote);
        line.push(indent);
        if has_indent {
            match &bullet {
                Some(b) => line.push(b.clone()),
                None => line.push(Span::raw(" ".repeat(last_w))),
            }
        }
        let content_start = line.iter().map(|span| display_width(&span.content)).sum::<usize>();
        for hit in hits {
            if hit.end_col > hit.start_col {
                self.doc.image_hits.push(ImageHit {
                    image: hit.image,
                    line: line_no,
                    start_col: content_start + hit.start_col,
                    end_col: content_start + hit.end_col,
                    part: hit.part,
                });
            }
        }
        line.append(&mut spans);
        self.doc.lines.push(Line::from(line));
    }

    fn blank(&mut self) {
        // 引用块内至少留一行（带竖线），其余按段距
        let gap = if self.quote_depth > 0 {
            self.style.para_gap.max(1)
        } else {
            self.style.para_gap
        };
        for _ in 0..gap {
            self.push_blank();
        }
    }

    /// 一个空行；引用块内保留竖线与缩进，维持引用条视觉连续。
    fn push_blank(&mut self) {
        if self.quote_depth > 0 {
            let mut line = self.quote_prefix();
            line.push(Span::raw(" ".repeat(self.pre_w() + self.last_w())));
            self.doc.lines.push(Line::from(line));
        } else {
            self.doc.lines.push(Line::raw(String::new()));
        }
    }
}

/// 行距后处理：对 lines[start..] 单遍重建，在相邻两条"非空行"之间插入
/// `gap` 个空行；排除行（代码块/表格/分割线，绝对索引在 `excluded` 中）内部
/// 以及排除行与相邻行之间不插。返回旧相对索引 -> 新绝对索引的映射。
fn apply_line_gap(
    doc: &mut Document,
    start: usize,
    excluded: &[usize],
    gap: u8,
) -> Vec<usize> {
    let end = doc.lines.len();
    let mut map = vec![0usize; end - start];
    let is_excluded = |i: usize| excluded.contains(&i);
    let is_blank = |l: &Line| l.spans.iter().all(|s| s.content.is_empty());
    let mut out: Vec<Line<'static>> = Vec::with_capacity(end - start);
    for i in start..end {
        let pair_next = i + 1 < end
            && !is_blank(&doc.lines[i])
            && !is_blank(&doc.lines[i + 1])
            && !is_excluded(i)
            && !is_excluded(i + 1);
        map[i - start] = start + out.len();
        out.push(doc.lines[i].clone());
        if pair_next {
            for _ in 0..gap {
                out.push(Line::raw(String::new()));
            }
        }
    }
    doc.lines.truncate(start);
    doc.lines.extend(out);
    map
}

fn heading_color(t: &Theme, level: u8) -> Color {
    match level {
        1 => t.h1,
        2 => t.h2,
        3 => t.h3,
        4 => t.h4,
        5 => t.h5,
        _ => t.h6,
    }
}

/// 标题层级前缀：块状字符由大到小，模拟字号差异。
fn heading_prefix(t: &Theme, level: u8) -> Span<'static> {
    let glyph = match level {
        1 => "█ ",
        2 => "▓ ",
        3 => "▒ ",
        4 => "░ ",
        5 => "▪ ",
        _ => "· ",
    };
    Span::styled(glyph, Style::new().fg(heading_color(t, level)))
}

fn heading_style(t: &Theme, level: u8) -> Style {
    let mut s = Style::new().fg(heading_color(t, level));
    if level <= 3 {
        s = s.add_modifier(Modifier::BOLD);
    }
    if level == 1 {
        s = s.add_modifier(Modifier::UNDERLINED);
    }
    s
}

/// 表格：计算列宽（必要时收窄到终端宽度），输出 box-drawing 边框与对齐的单元格。
fn render_table(ctx: &mut Ctx, t: &TableState) {
    let n = t.aligns.len();
    if n == 0 {
        return;
    }
    let border = Style::new().fg(ctx.theme.border);

    let mut widths: Vec<usize> = vec![3; n];
    let measure = |cells: &Vec<String>, widths: &mut Vec<usize>| {
        for (i, w) in widths.iter_mut().enumerate().take(n) {
            let cell = cells.get(i).map(String::as_str).unwrap_or("");
            *w = (*w).max(display_width(cell));
        }
    };
    if let Some(h) = &t.header {
        measure(h, &mut widths);
    }
    for (cells, _) in &t.rows {
        measure(cells, &mut widths);
    }
    for w in &mut widths {
        *w = (*w).max(3);
    }

    // 收窄到终端宽度：按比例缩放（下限 3），仍超宽时由终端截断
    let fixed = n + 1 + 2 * n;
    let natural: usize = widths.iter().sum();
    if fixed + natural > ctx.width {
        let avail = (ctx.width - fixed).max(3 * n);
        for w in &mut widths {
            *w = (*w * avail / natural).max(3);
        }
    }

    let bar = |l: &str, m: &str, r: &str| -> Vec<Span<'static>> {
        let mut s = String::from(l);
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&"─".repeat(w + 2));
            s.push_str(if i + 1 == widths.len() { r } else { m });
        }
        vec![Span::styled(s, border)]
    };
    ctx.raw_line(bar("┌", "┬", "┐"));

    let mut all_rows: Vec<TableRenderRow<'_>> = Vec::new();
    if let Some(h) = &t.header {
        let images = t.header_images.as_deref().unwrap_or(&[]);
        all_rows.push(TableRenderRow {
            cells: h,
            images,
            is_head: true,
        });
    }
    for (cells, images) in &t.rows {
        all_rows.push(TableRenderRow {
            cells,
            images,
            is_head: false,
        });
    }
    for row in &all_rows {
        let cells = row.cells;
        let cell_images = row.images;
        let is_head = row.is_head;
        let wrapped: Vec<Vec<String>> = (0..n)
            .map(|i| {
                let cell = cells.get(i).map(String::as_str).unwrap_or("");
                wrap_str(cell, widths[i])
            })
            .collect();
        let mappings: Vec<Vec<Vec<WrapMapping>>> = (0..n)
            .map(|i| {
                let cell = cells.get(i).map(String::as_str).unwrap_or("");
                wrap_mappings(cell, widths[i])
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
        for r in 0..height {
            let mut spans = vec![Span::styled("│", border)];
            let mut visual_col = 1usize;
            let mut table_hits = Vec::new();
            for i in 0..n {
                let text = wrapped[i].get(r).cloned().unwrap_or_default();
                let align = t.aligns.get(i).copied();
                let pad = widths[i].saturating_sub(display_width(&text).min(widths[i]));
                let left_pad = match align {
                    Some(Alignment::Right) => pad,
                    Some(Alignment::Center) => pad / 2,
                    _ => 0,
                };
                if let Some(images) = cell_images.get(i)
                    && let Some(line_mappings) = mappings.get(i).and_then(|m| m.get(r))
                {
                    for image in images {
                        for mapping in line_mappings {
                            let start = image.start_col.max(mapping.source_start);
                            let end = image.end_col.min(mapping.source_end);
                            if start < end {
                                table_hits.push(TableImageHit {
                                    image: image.image,
                                    start_col: visual_col
                                        + left_pad
                                        + mapping.output_start
                                        + start
                                        - mapping.source_start,
                                    end_col: visual_col
                                        + left_pad
                                        + mapping.output_start
                                        + end
                                        - mapping.source_start,
                                    part: image.part,
                                });
                            }
                        }
                    }
                }
                let style = if is_head {
                    Style::new().add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                spans.push(Span::raw(" "));
                spans.push(Span::styled(align_cell(&text, widths[i], align), style));
                spans.push(Span::styled(" │", border));
                visual_col += 1 + widths[i] + 2;
            }
            ctx.raw_line_with_hits(spans, &table_hits);
        }
        if is_head {
            ctx.raw_line(bar("├", "┼", "┤"));
        }
    }
    ctx.raw_line(bar("└", "┴", "┘"));
}

fn align_cell(text: &str, w: usize, align: Option<Alignment>) -> String {
    let tw = display_width(text).min(w);
    let pad = w - tw;
    match align {
        Some(Alignment::Right) => format!("{}{}", " ".repeat(pad), text),
        Some(Alignment::Center) => {
            let l = pad / 2;
            format!("{}{}{}", " ".repeat(l), text, " ".repeat(pad - l))
        }
        _ => format!("{}{}", text, " ".repeat(pad)),
    }
}

fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn is_windows_drive_path(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_local_image_path(raw: &str) -> bool {
    if raw.starts_with("//") {
        return false;
    }
    is_windows_drive_path(raw) || raw.find(':').is_none()
}

fn resolve_local_path(raw: &str, base: Option<&Path>) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let path = Path::new(raw);
    if path.is_absolute() || is_windows_drive_path(raw) {
        Some(path.to_path_buf())
    } else {
        base.map(|dir| dir.join(path))
    }
}

/// 提取 `<summary>` 的显示文本，去除标签。
fn extract_summary(raw: &str) -> String {
    let mut s = raw.to_string();
    let lower = s.to_lowercase();
    if let Some(p) = lower.find("<summary") {
        let rest = &s[p..];
        if let Some(gt) = rest.find('>') {
            s = rest[gt + 1..].to_string();
        }
    }
    if let Some(p) = s.to_lowercase().find("</summary") {
        s.truncate(p);
    }
    strip_tags(&s).trim().to_string()
}

/// 粗略移除 `<...>` 标签序列。
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// 制表符按 4 列展开，避免渲染与终端丢失缩进（Tab 在渲染层宽度为 0）。
fn expand_tabs(s: &str) -> Cow<'_, str> {
    if s.contains('\t') {
        Cow::Owned(s.replace('\t', "    "))
    } else {
        Cow::Borrowed(s)
    }
}

/// 把行内片段切成可断行的 token：空格后断、CJK 等宽字符后断。
fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0usize;
    for (i, ch) in text.char_indices() {
        if ch == ' ' || ch.width().unwrap_or(0) >= 2 {
            let end = i + ch.len_utf8();
            tokens.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        tokens.push(&text[start..]);
    }
    tokens
}

fn push_marked_span(line: &mut Vec<MarkedSpan>, marked: ImageMarker, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = line.last_mut()
        && last.span.style == style
        && last.image == Some(marked)
    {
        last.span.content.to_mut().push_str(text);
        return;
    }
    line.push(MarkedSpan {
        span: Span::styled(text.to_string(), style),
        image: Some(marked),
    });
}

fn push_plain_or_marked(
    line: &mut Vec<MarkedSpan>,
    image: Option<ImageMarker>,
    text: &str,
    style: Style,
) {
    if text.is_empty() {
        return;
    }
    if let Some(marker) = image {
        push_marked_span(line, marker, text, style);
    } else if let Some(last) = line.last_mut()
        && last.span.style == style
        && last.image.is_none()
    {
        last.span.content.to_mut().push_str(text);
    } else {
        line.push(MarkedSpan::plain(Span::styled(text.to_string(), style)));
    }
}

fn wrap_marked_spans(spans: &[MarkedSpan], budget: usize) -> Vec<Vec<MarkedSpan>> {
    let budget = budget.max(1);
    let mut lines: Vec<Vec<MarkedSpan>> = vec![Vec::new()];
    let mut cur_w = 0usize;
    for marked in spans {
        let style = marked.span.style;
        for token in tokenize(&marked.span.content) {
            let tw = display_width(token);
            if cur_w + tw <= budget {
                push_plain_or_marked(lines.last_mut().unwrap(), marked.image, token, style);
                cur_w += tw;
            } else if tw > budget {
                for ch in token.chars() {
                    if ch == ' ' && cur_w == 0 {
                        continue;
                    }
                    let cw = ch.width().unwrap_or(0);
                    if cur_w + cw > budget {
                        lines.push(Vec::new());
                        cur_w = 0;
                    }
                    push_plain_or_marked(
                        lines.last_mut().unwrap(),
                        marked.image,
                        &ch.to_string(),
                        style,
                    );
                    cur_w += cw;
                }
            } else {
                lines.push(Vec::new());
                cur_w = 0;
                let t = token.trim_start_matches(' ');
                if !t.is_empty() {
                    push_plain_or_marked(lines.last_mut().unwrap(), marked.image, t, style);
                    cur_w = display_width(t);
                }
            }
        }
    }
    lines
}

/// 贪心折行：优先在空格断行；超宽单词按字符硬断。续行剔除行首空格。
pub(crate) fn wrap_spans(spans: &[Span<'static>], budget: usize) -> Vec<Vec<Span<'static>>> {
    let marked: Vec<MarkedSpan> = spans.iter().cloned().map(MarkedSpan::plain).collect();
    wrap_marked_spans(&marked, budget)
        .into_iter()
        .map(|line| line.into_iter().map(|marked| marked.span).collect())
        .collect()
}

#[derive(Clone, Copy)]
struct WrapMapping {
    source_start: usize,
    source_end: usize,
    output_start: usize,
}

fn wrap_mappings(text: &str, budget: usize) -> Vec<Vec<WrapMapping>> {
    let budget = budget.max(1);
    let mut lines: Vec<Vec<WrapMapping>> = vec![Vec::new()];
    let mut cur_w = 0usize;
    let mut source_col = 0usize;
    for token in tokenize(text) {
        let tw = display_width(token);
        if cur_w + tw <= budget {
            if tw > 0 {
                lines.last_mut().unwrap().push(WrapMapping {
                    source_start: source_col,
                    source_end: source_col + tw,
                    output_start: cur_w,
                });
            }
            cur_w += tw;
            source_col += tw;
        } else if tw > budget {
            for ch in token.chars() {
                let cw = ch.width().unwrap_or(0);
                if ch == ' ' && cur_w == 0 {
                    source_col += cw;
                    continue;
                }
                if cur_w + cw > budget {
                    lines.push(Vec::new());
                    cur_w = 0;
                }
                if cw > 0 {
                    lines.last_mut().unwrap().push(WrapMapping {
                        source_start: source_col,
                        source_end: source_col + cw,
                        output_start: cur_w,
                    });
                }
                cur_w += cw;
                source_col += cw;
            }
        } else {
            lines.push(Vec::new());
            let trimmed = token.trim_start_matches(' ');
            let skipped = display_width(&token[..token.len() - trimmed.len()]);
            let trimmed_width = display_width(trimmed);
            if trimmed_width > 0 {
                lines.last_mut().unwrap().push(WrapMapping {
                    source_start: source_col + skipped,
                    source_end: source_col + tw,
                    output_start: 0,
                });
            }
            cur_w = trimmed_width;
            source_col += tw;
        }
    }
    lines
}

fn wrap_str(s: &str, budget: usize) -> Vec<String> {
    wrap_spans(&[Span::raw(s.to_string())], budget)
        .into_iter()
        .map(|line| line.iter().map(|sp| sp.content.as_ref()).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::DARK;

    fn renderer() -> Renderer {
        Renderer::new(DARK)
    }

    /// 便捷入口：安全区 0、紧凑行距、标准段距。
    fn render(src: &str, width: u16) -> Document {
        renderer().render_with(src, width, RenderStyle {
            safe_zone: 0,
            ..Default::default()
        })
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn doc_lines(doc: &Document) -> Vec<String> {
        doc.lines.iter().map(text).collect()
    }

    #[test]
    fn heading_style_and_toc() {
        let doc = render("# 标题一\n\n正文\n", 80);
        assert_eq!(doc.toc.len(), 1);
        assert_eq!(doc.toc[0].level, 1);
        assert_eq!(doc.toc[0].title, "标题一");
        assert_eq!(doc.toc[0].line, 0);
        assert!(doc.lines[0].spans.iter().any(|s| s.content.contains("标题一")));
        let h1 = doc
            .lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("标题一"))
            .unwrap();
        assert_eq!(h1.style.fg, Some(DARK.h1));
        assert!(h1.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn latin_wrap_breaks_on_spaces() {
        let doc = render("aaaa bbbb cccc dddd eeee ffff gggg hhhh\n", 24);
        for l in &doc.lines {
            assert!(display_width(&text(l)) <= 24, "超宽行: {:?}", text(l));
        }
        let joined = doc_lines(&doc).join("\n");
        assert!(joined.contains("aaaa"));
        assert!(joined.contains("hhhh"));
        // 续行不保留行首空格
        assert!(doc_lines(&doc).iter().skip(1).all(|l| !l.starts_with(' ')));
    }

    #[test]
    fn cjk_wraps_by_display_width() {
        let doc = render("汉字测试段落内容超过宽度限制继续加长文本\n", 30);
        let lines = doc_lines(&doc);
        assert!(lines.len() >= 2);
        for l in &lines {
            assert!(display_width(l) <= 30, "CJK 超宽: {l}");
        }
    }

    #[test]
    fn unordered_list_marker_and_alignment() {
        let doc = render("- 项目甲内容很长很长很长需要折行显示\n- 项目乙\n", 20);
        let lines = doc_lines(&doc);
        assert_eq!(lines[0], "• 项目甲内容很长很长");
        assert_eq!(lines[1], "  很长需要折行显示", "续行按标记宽度对齐");
        assert!(lines.iter().any(|l| l.contains("• 项目乙")));
    }

    #[test]
    fn nested_list_indents() {
        let doc = render("- 外层\n  - 内层\n", 40);
        let lines = doc_lines(&doc);
        assert!(lines[0].contains("• 外层"));
        let inner = lines.iter().find(|l| l.contains("内层")).unwrap();
        assert!(inner.starts_with("  • "), "内层缩进: {inner:?}");
    }

    #[test]
    fn ordered_list_numbering() {
        let doc = render("1. 第一\n2. 第二\n", 40);
        let lines = doc_lines(&doc);
        assert!(lines[0].contains("1. 第一"));
        assert!(lines.iter().any(|l| l.contains("2. 第二")));
    }

    #[test]
    fn task_list_checkboxes() {
        let doc = render("- [x] 已完成\n- [ ] 待办\n", 40);
        let joined = doc_lines(&doc).join("\n");
        assert!(joined.contains("[x] 已完成"));
        assert!(joined.contains("[ ] 待办"));
    }

    #[test]
    fn blockquote_prefix() {
        let doc = render("> 引用文本\n", 40);
        let lines = doc_lines(&doc);
        assert!(lines[0].starts_with("│ "), "引用前缀: {lines:?}");
        assert!(lines[0].contains("引用文本"));
    }

    #[test]
    fn inline_code_has_background() {
        let doc = render("使用 `cargo build` 构建\n", 40);
        let line = doc.lines.iter().find(|l| text(l).contains("cargo build")).unwrap();
        assert!(line.spans.iter().any(|s| s.style.bg.is_some()));
    }

    #[test]
    fn link_shows_url_dimmed() {
        let doc = render("[点击](https://example.com)\n", 40);
        let line = doc.lines.iter().find(|l| text(l).contains("点击")).unwrap();
        let t = text(line);
        assert!(t.contains("(https://example.com)"), "URL 展示: {t}");
        assert!(line.spans.iter().any(|s| s.style.add_modifier.contains(Modifier::UNDERLINED)));
    }

    #[test]
    fn strikethrough_modifier() {
        let doc = render("~~删除线~~\n", 40);
        let line = doc.lines.iter().find(|l| text(l).contains("删除线")).unwrap();
        assert!(line.spans.iter().any(|s| s.style.add_modifier.contains(Modifier::CROSSED_OUT)));
    }

    #[test]
    fn fenced_code_highlighted_with_background() {
        let doc = render("```rust\nlet x = 1;\n```\n", 40);
        assert!(doc.lines.iter().any(|l| {
            text(l).contains("let x") && l.spans.iter().any(|s| s.style.bg.is_some())
        }));
    }

    #[test]
    fn unknown_code_lang_falls_back_to_plain_text() {
        let doc = render("```nosuchlang\nplain text here\n```\n", 40);
        assert!(doc.lines.iter().any(|l| text(l).contains("plain text here")));
    }

    #[test]
    fn table_renders_borders_and_header_bold() {
        let src = "| 名称 | 数量 |\n|------|-----:|\n| 苹果 | 3 |\n";
        let doc = render(src, 40);
        let lines = doc_lines(&doc);
        assert!(lines.iter().any(|l| l.starts_with('┌')));
        assert!(lines.iter().any(|l| l.contains('├')));
        assert!(lines.iter().any(|l| l.starts_with('└')));
        let header = lines.iter().find(|l| l.contains("名称")).unwrap();
        assert!(header.contains("│"));
        let row = lines.iter().find(|l| l.contains("苹果")).unwrap();
        assert!(row.contains("3"), "右对齐列: {row}");
    }

    #[test]
    fn rule_renders_dashes() {
        let doc = render("上\n\n---\n\n下\n", 40);
        assert!(doc.lines.iter().any(|l| text(l).starts_with('─')));
    }

    #[test]
    fn heading_glyphs_shrink_with_level() {
        let doc = render("# 一级\n\n## 二级\n\n### 三级\n\n#### 四级\n", 40);
        let lines = doc_lines(&doc);
        assert!(lines[0].starts_with("█ "), "H1: {lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("▓ ")), "H2");
        assert!(lines.iter().any(|l| l.starts_with("▒ ")), "H3");
        assert!(lines.iter().any(|l| l.starts_with("░ ")), "H4");
    }

    #[test]
    fn code_block_safe_zone_and_uniform_bg() {
        let doc = renderer().render_with(
            "```text\nhello\n```\n",
            40,
            RenderStyle {
                safe_zone: 2,
                ..Default::default()
            },
        );
        let hi = doc
            .lines
            .iter()
            .position(|l| text(l).contains("hello"))
            .expect("代码行存在");
        // 代码行上所有非空前缀片段底色一致（字符处 = 空白处）
        let bgs: Vec<_> = doc.lines[hi]
            .spans
            .iter()
            .filter(|s| !s.content.is_empty())
            .map(|s| s.style.bg)
            .collect();
        assert!(!bgs.is_empty());
        assert!(bgs.iter().all(|b| b.is_some()), "字符处也应有底色: {bgs:?}");
        assert!(bgs.iter().all(|b| *b == bgs[0]), "底色应统一: {bgs:?}");
        // 上下各有一行整行底色安全区
        for row in [hi - 1, hi + 1] {
            let spans = &doc.lines[row].spans;
            assert!(
                spans.iter().any(|s| s.style.bg.is_some() && s.content.chars().count() >= 30),
                "第 {row} 行应为底色安全区: {:?}",
                text(&doc.lines[row])
            );
        }
    }

    #[test]
    fn line_gap_inserts_between_adjacent_body_lines() {
        let doc = renderer().render_with(
            "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii\n",
            16,
            RenderStyle {
                safe_zone: 0,
                line_gap: 1,
                para_gap: 0,
            },
        );
        let lines = doc_lines(&doc);
        assert!(lines.len() >= 5, "{lines:?}");
        // 相邻非空正文行之间恰好 1 个空行，段尾无空行
        assert_eq!(lines.last().unwrap(), &lines.iter().rfind(|l| !l.is_empty()).unwrap().clone());
        for w in lines.windows(2) {
            if !w[0].is_empty() && !w[1].is_empty() {
                panic!("相邻正文行之间应有空行: {lines:?}");
            }
        }
    }

    #[test]
    fn line_gap_skips_code_card_lines() {
        let src = "```rust\nlet x = 1;\n```\n\n正文段落\n";
        let doc = renderer().render_with(
            src,
            40,
            RenderStyle {
                safe_zone: 1,
                line_gap: 1,
                para_gap: 1,
            },
        );
        let lines = doc_lines(&doc);
        let card: Vec<usize> = doc
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                !l.spans.is_empty()
                    && l.spans.iter().filter(|s| !s.content.is_empty()).all(|s| s.style.bg.is_some())
            })
            .map(|(i, _)| i)
            .collect();
        assert!(!card.is_empty(), "代码卡片行应有底色: {lines:?}");
        // 卡片行不参与行距：与 line_gap=0 的输出完全一致
        let tight = renderer().render_with(
            src,
            40,
            RenderStyle {
                safe_zone: 1,
                line_gap: 0,
                para_gap: 1,
            },
        );
        assert_eq!(lines, doc_lines(&tight), "卡片周围不应插空行");
    }

    #[test]
    fn code_card_top_row_shows_lang_tag() {
        let doc = renderer().render_with(
            "```rust\nlet x = 1;\n```\n",
            40,
            RenderStyle {
                safe_zone: 2,
                ..Default::default()
            },
        );
        let hi = doc
            .lines
            .iter()
            .position(|l| text(l).contains("let x"))
            .expect("代码行存在");
        let top = &doc.lines[hi - 1];
        assert!(text(top).contains(" rust "), "顶部行应含语言标签: {:?}", text(top));
        // 整行底色统一
        let bgs: Vec<_> = top.spans.iter().filter(|s| !s.content.is_empty()).map(|s| s.style.bg).collect();
        assert!(bgs.iter().all(|b| b.is_some() && *b == bgs[0]), "{bgs:?}");
        // 整行总宽 = budget
        let w: usize = top.spans.iter().map(|s| display_width(&s.content)).sum();
        assert_eq!(w, 40, "整行宽应为 budget: {:?}", text(top));
        // 未知语言（缩进代码块）仍是纯空白行
        let doc = renderer().render_with(
            "```\nplain\n```\n",
            40,
            RenderStyle {
                safe_zone: 1,
                ..Default::default()
            },
        );
        let hi = doc
            .lines
            .iter()
            .position(|l| text(l).contains("plain"))
            .expect("代码行存在");
        let top = &doc.lines[hi - 1];
        assert_eq!(text(top), " ".repeat(40), "无语言时顶部行应纯空白: {:?}", text(top));
    }

    #[test]
    fn toc_line_resolved_after_line_gap_insertion() {
        let doc = renderer().render_with(
            "段落内容较长在窄宽度下折行\n\n## 二级标题\n",
            12,
            RenderStyle {
                safe_zone: 0,
                line_gap: 1,
                para_gap: 1,
            },
        );
        assert!(doc.line_count() > 3);
        let entry = doc.toc.last().unwrap();
        assert_ne!(entry.line, UNRESOLVED_LINE);
        assert!(
            text(&doc.lines[entry.line]).contains("二级标题"),
            "大纲行号应指向插入空行后的新位置: {:?}",
            entry
        );
    }

    #[test]
    fn split_chunks_cuts_at_blank_lines() {
        let src = "段落内容\n\n".repeat(1000);
        let chunks = split_chunks(&src);
        assert!(chunks.len() >= 2);
        for c in chunks.iter().rev().skip(1) {
            assert!(c.lines().count() >= CHUNK_LINES);
            assert!(c.ends_with("\n\n"), "切块应终止于空行");
        }
        let total: usize = chunks.iter().map(|c| c.lines().count()).sum();
        assert_eq!(total, 2000);
    }

    #[test]
    fn scan_headings_skips_fences() {
        let chunks = vec!["# 真标题\n\n```rust\n# 不是标题\n```\n\n正文\n".to_string()];
        let out = scan_headings(&chunks);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, 1);
        assert_eq!(out[0].2, "真标题");
    }

    #[test]
    fn lazy_render_progresses_and_resolves_toc() {
        let mut src = String::new();
        for i in 0..21000 {
            if i % 5000 == 0 {
                src.push_str(&format!("# 章 {i}\n\n"));
            }
            src.push_str("这一段是用于撑起大文件的中文内容。\n\n");
        }
        assert!(src.len() > LAZY_THRESHOLD);
        let r = renderer();
        let mut doc = r.render_with(&src, 60, RenderStyle::default());
        assert!(!doc.fully_rendered(), "大文件应懒渲染");
        assert_eq!(doc.toc.len(), 5);
        assert_ne!(doc.toc[0].line, UNRESOLVED_LINE, "首块标题应已解析");
        assert_eq!(
            doc.toc.last().unwrap().line,
            UNRESOLVED_LINE,
            "后续块尚未渲染"
        );
        while r.render_next_chunk(&mut doc, 60, RenderStyle::default()) {}
        assert!(doc.fully_rendered());
        assert!(doc.toc.iter().all(|e| e.line != UNRESOLVED_LINE));
        assert!(doc.toc.iter().all(|e| e.line < doc.line_count()));
    }

    #[test]
    fn code_block_tab_indent_preserved() {
        let doc = renderer().render_with(
            "```go\nfunc f() {\n\treturn 1\n}\n```\n",
            40,
            RenderStyle {
                safe_zone: 0,
                ..Default::default()
            },
        );
        let line = doc
            .lines
            .iter()
            .find(|l| text(l).contains("return"))
            .expect("代码行存在");
        assert!(
            text(line).starts_with("    return"),
            "Tab 应展开为 4 空格: {:?}",
            text(line)
        );
    }

    #[test]
    fn quote_blank_lines_keep_bar() {
        let doc = renderer().render_with(
            "> 段落一\n>\n> 段落二\n",
            40,
            RenderStyle {
                safe_zone: 0,
                ..Default::default()
            },
        );
        // 每段结束后的空行也带竖线，引用条全程连续
        let lines = doc_lines(&doc);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(
            lines.iter().all(|l| l.starts_with("│")),
            "引用条不应断开: {lines:?}"
        );
    }

    #[test]
    fn para_gap_and_line_gap_spacing() {
        // 段距 0：段落间无空行
        let tight = renderer().render_with(
            "甲段落\n\n乙段落\n",
            40,
            RenderStyle {
                safe_zone: 0,
                line_gap: 0,
                para_gap: 0,
            },
        );
        assert_eq!(tight.line_count(), 2, "{:?}", doc_lines(&tight));
        // 行距 1：折行之间插入空行
        let loose = renderer().render_with(
            "这一段中文足够长一定会折行换行\n",
            12,
            RenderStyle {
                safe_zone: 0,
                line_gap: 1,
                para_gap: 1,
            },
        );
        let lines = doc_lines(&loose);
        let content: Vec<&String> = lines.iter().filter(|l| !l.is_empty()).collect();
        assert!(content.len() >= 2, "{lines:?}");
        // 折行间 1 空行 + 段尾 1 空行
        assert_eq!(
            lines.len(),
            content.len() + (content.len() - 1) + 1,
            "折行间应有空行: {lines:?}"
        );
    }

    #[test]
    fn details_content_lines_not_swallowed() {
        // 无空行紧跟 summary 的内容行：pulldown 会把它并进同一个 HTML 事件，
        // 逐行处理后不应被吞，且折叠区间覆盖内容
        let src = "<details>\n<summary>点我展开</summary>\n这里是隐藏内容，支持 **Markdown**。\n\n- 列表项\n\n</details>\n结尾\n";
        let doc = render(src, 40);
        let joined: String = doc
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect();
        assert!(joined.contains("点我展开"), "{joined}");
        assert!(joined.contains("这里是隐藏内容"), "内容行不应被吞: {joined}");
        assert!(joined.contains("列表项"));
        assert_eq!(doc.collapsibles.len(), 1);
        let c = &doc.collapsibles[0];
        assert!(c.end > c.line);
        let in_range: String = doc.lines[c.line..c.end]
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect();
        assert!(in_range.contains("这里是隐藏内容"), "区间应覆盖内容: {in_range}");
    }

    #[test]
    fn toc_line_points_at_heading_line() {
        let src = "段落\n\n## 二级标题\n\n更多\n";
        let doc = render(src, 40);
        assert_eq!(doc.toc.len(), 1);
        let entry = &doc.toc[0];
        assert_eq!(entry.level, 2);
        assert!(text(&doc.lines[entry.line]).contains("二级标题"));
    }

    #[test]
    fn resize_wide_then_narrow_is_consistent() {
        let src = "这一段中文内容在两种宽度下都应当被正确折行而不panic\n";
        let wide = render(src, 200);
        let narrow = render(src, 20);
        assert!(wide.line_count() < narrow.line_count());
    }

    #[test]
    fn image_metadata_keeps_alt_and_path_as_separate_hits() {
        let doc = render("![相同](相同.png) ![](empty.png)\n", 80);
        assert_eq!(doc.images.len(), 2);
        assert!(doc.lines.iter().any(|line| text(line).contains("相同 (相同.png)")));
        assert!(doc.lines.iter().any(|line| text(line).contains(" (empty.png)")));
        assert_eq!(
            doc.image_hits
                .iter()
                .filter(|hit| hit.image == 0 && hit.part == ImageHitPart::Alt)
                .count(),
            1
        );
        assert_eq!(
            doc.image_hits
                .iter()
                .filter(|hit| hit.image == 0 && hit.part == ImageHitPart::Path)
                .count(),
            1
        );
        assert!(doc.image_hits.iter().any(|hit| {
            hit.image == 1 && hit.part == ImageHitPart::Path && hit.end_col > hit.start_col
        }));
        let first = doc.image_hits.iter().find(|hit| hit.image == 0 && hit.part == ImageHitPart::Alt).unwrap();
        let path = doc.image_hits.iter().find(|hit| hit.image == 0 && hit.part == ImageHitPart::Path).unwrap();
        assert!(first.end_col <= path.start_col);
    }

    #[test]
    fn image_hits_wrap_with_unicode_and_line_gap() {
        let doc = renderer().render_with(
            "![中文😀](图片/😀.png) 普通正文 普通正文 普通正文\n",
            20,
            RenderStyle {
                safe_zone: 0,
                line_gap: 1,
                para_gap: 0,
            },
        );
        assert!(doc.image_hits.len() >= 2, "alt/path 都应有命中: {:?}", doc.image_hits);
        for hit in &doc.image_hits {
            assert!(hit.end_col > hit.start_col);
            assert!(hit.line < doc.lines.len());
            assert!(hit.end_col <= display_width(&text(&doc.lines[hit.line])));
            assert!(!text(&doc.lines[hit.line]).is_empty());
        }
        assert!(doc
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| text(line).is_empty())
            .all(|(line, _)| !doc.image_hits.iter().any(|hit| hit.line == line)));
    }

    #[test]
    fn image_protocols_are_not_local_and_windows_drive_is_not_uri() {
        let mut doc = render(
            "![远程](https://example.invalid/a.png) ![文件](file:///tmp/a.png) ![盘](C:/img/a.png)\n",
            80,
        );
        doc.resolve_image_paths(Some(Path::new("/tmp/doc/readme.md")));
        assert_eq!(doc.images.len(), 3);
        assert!(!doc.images[0].is_local);
        assert!(doc.images[0].resolved_path.is_none());
        assert!(!doc.images[1].is_local);
        assert!(doc.images[2].is_local);
        assert_eq!(doc.images[2].resolved_path, Some(PathBuf::from("C:/img/a.png")));
    }

    #[test]
    fn table_image_keeps_text_and_reference() {
        let doc = render("| 图 |\n|---|\n| ![图](images/a.png) |\n", 60);
        assert_eq!(doc.images.len(), 1);
        let joined = doc_lines(&doc).join("\\n");
        assert!(joined.contains("图 (images/a.png)"), "表格图片文字不应丢失: {joined}");
        assert_eq!(
            doc.image_hits.iter().filter(|hit| hit.image == 0).count(),
            2,
            "表格中的 alt/path 也应保留命中元数据: {:?}",
            doc.image_hits
        );
        let wrapped = render(
            "| 图 |\n|---|\n| ![中文](images/a-very-long-image-name.png) |\n",
            20,
        );
        assert!(
            wrapped.image_hits.len() >= 2,
            "表格图片折行后仍应可命中: {:?}",
            wrapped.image_hits
        );
        assert!(wrapped.image_hits.iter().all(|hit| {
            hit.end_col <= display_width(&text(&wrapped.lines[hit.line]))
        }));
    }

    #[test]
    fn kitchen_sink_renders_at_multiple_widths() {
        let src = "# red 使用手册\n\n## 简介\n\n**red** 是一个终端 *Markdown* 阅读器，~~尝试~~专注阅读体验。\n\n### 特性\n\n- 支持 `行内代码` 与围栏代码\n- [x] 表格\n- [ ] 数学公式\n  1. 嵌套有序列表\n  2. 第二项\n\n> 引用块支持\n> 多行内容\n\n```rust\nfn main() {\n    let msg = \"你好，世界\";\n    println!(\"{msg}\");\n}\n```\n\n| 功能 | 状态 | 备注 |\n|:-----|:----:|-----:|\n| 渲染 | 完成 | GFM 扩展 |\n| 目录 | 进行中 | M3 交付 |\n\n详见 [GitHub](https://github.com) 或 ![图标](https://img.png)。\n\n---\n\n完。\n";
        for width in [20u16, 40, 80, 120, 200] {
            let doc = render(src, width);
            assert!(doc.line_count() > 10, "width {width} 行数异常");
            assert_eq!(doc.toc.len(), 3, "width {width} 大纲异常");
            assert!(doc.toc.iter().all(|e| e.line < doc.line_count()));
        }
    }
}

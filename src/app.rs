use std::cmp::min;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::text::Line;
use ratatui::DefaultTerminal;

#[cfg(feature = "native-images")]
use image::DynamicImage;
#[cfg(feature = "native-images")]
use ratatui_image::{Resize, picker::{Capability, Picker, ProtocolType}, protocol::Protocol};

use crate::browser;
use crate::config;
use crate::markdown::{Document, ImageRef, Renderer, UNRESOLVED_LINE};
use crate::theme::Theme;
use crate::ui;
use crate::watcher::FileWatcher;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 阅读模式
    Read,
    /// 大纲侧栏
    Toc,
    /// 搜索词输入
    SearchInput,
    /// 设置面板
    Settings,
    /// 安全区列数自定义输入
    ZoneInput,
    /// 目录浏览器
    Browse,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ImagePanel {
    #[default]
    Closed,
    PathOnly { image: ImageRef },
}

#[cfg(feature = "native-images")]
#[derive(Clone)]
pub struct NativeImageCache {
    pub image_id: usize,
    pub raw_path: String,
    pub source: DynamicImage,
    pub protocol: Protocol,
}

pub struct App {
    pub theme: Theme,
    pub title: String,
    /// 原始 Markdown 源文，宽度变化时用于重渲染
    pub source: String,
    pub renderer: Renderer,
    pub doc: Document,
    /// 当前渲染所用的宽度（列数）
    pub render_width: u16,
    /// 视口顶部的行号
    pub scroll: usize,
    /// 正文区可视行数（总高度减去状态栏），由 resize 维护
    pub view_height: usize,
    pub mode: Mode,
    pub image_panel: ImagePanel,
    #[cfg(feature = "native-images")]
    pub native_picker: Option<Picker>,
    #[cfg(feature = "native-images")]
    pub native_image: Option<NativeImageCache>,
    pub quit: bool,
    /// 大纲当前选中项
    pub toc_selected: usize,
    /// 设置面板当前选中项
    pub settings_sel: usize,
    /// 安全区宽度（列）：0=关、1-2；作用于正文左右与代码块四周
    pub safe_zone: u8,
    /// 行距：0=紧凑 1=宽松（段落内折行间空行）
    pub line_gap: u8,
    /// 段距：0=紧凑 1=标准 2=宽松（块间空行数）
    pub para_gap: u8,
    /// 折叠块默认状态：false=展开 true=收起
    pub fold_default: bool,
    /// 折叠状态：按 doc.collapsibles 索引对齐，true=该折叠块收起
    pub folds: Vec<bool>,
    /// 安全区列数自定义输入缓冲（ZoneInput 模式）
    pub zone_input: String,
    /// 搜索输入缓冲（SearchInput 模式）
    pub search_input: String,
    /// 已生效的搜索词
    pub search_query: Option<String>,
    /// 命中行号列表（升序）
    pub matches: Vec<usize>,
    /// 当前命中的序号
    pub match_idx: Option<usize>,
    /// 状态栏临时消息（如“未找到”）
    pub status_msg: Option<String>,
    /// 当前文件路径（live reload 与配置持久化的依据；stdin 时为 None）
    pub source_path: Option<PathBuf>,
    /// 文件变更监视器（--no-watch 或 stdin 时为 None）
    pub watcher: Option<FileWatcher>,
    /// 目录浏览器当前目录
    pub browse_dir: PathBuf,
    /// 目录浏览器条目
    pub browse_entries: Vec<browser::BrowserEntry>,
    /// 目录浏览器选中项
    pub browse_sel: usize,
    /// 从浏览器打开文件后，Esc 应回到浏览器而非退出
    pub came_from_browse: bool,
    /// 是否启用 live reload（--no-watch 关闭）
    pub watch_enabled: bool,
}

impl App {
    pub fn new(source: &str, title: String, theme: Theme) -> Self {
        let renderer = Renderer::new(theme);
        // 初始以 80 列渲染；进入事件循环后的首次 resize 会按真实宽度重渲染
        let mut app = Self {
            theme,
            title,
            source: source.to_string(),
            renderer,
            doc: Document::default(),
            render_width: 80,
            scroll: 0,
            view_height: 24,
            mode: Mode::Read,
            image_panel: ImagePanel::default(),
            #[cfg(feature = "native-images")]
            native_picker: None,
            #[cfg(feature = "native-images")]
            native_image: None,
            quit: false,
            toc_selected: 0,
            settings_sel: 0,
            safe_zone: 2,
            line_gap: 0,
            para_gap: 1,
            fold_default: false,
            folds: Vec::new(),
            zone_input: String::new(),
            search_input: String::new(),
            search_query: None,
            matches: Vec::new(),
            match_idx: None,
            status_msg: None,
            source_path: None,
            watcher: None,
            browse_dir: PathBuf::from("."),
            browse_entries: Vec::new(),
            browse_sel: 0,
            came_from_browse: false,
            watch_enabled: true,
        };
        app.rerender();
        app
    }

    #[cfg(feature = "native-images")]
    pub fn init_native_images(&mut self) {
        let Ok(picker) = Picker::from_query_stdio() else {
            return;
        };
        if picker_supports_native(&picker) {
            self.native_picker = Some(picker);
        }
    }

    #[cfg(feature = "native-images")]
    fn try_load_native_image(&mut self, image: &ImageRef) {
        self.native_image = None;
        let (Some(picker), Some(path), Some(size)) = (
            self.native_picker.as_ref(),
            image.resolved_path.as_ref(),
            ui::native_image_size(self),
        ) else {
            return;
        };
        let source = match image::open(path) {
            Ok(source) => source,
            Err(error) => {
                self.status_msg = Some(format!("图片无法读取，回退 PathOnly: {error}"));
                return;
            }
        };
        let protocol = match picker.new_protocol(source.clone(), size, Resize::Fit(None)) {
            Ok(protocol) => protocol,
            Err(error) => {
                self.status_msg = Some(format!("图片协议创建失败，回退 PathOnly: {error}"));
                return;
            }
        };
        self.native_image = Some(NativeImageCache {
            image_id: image.id,
            raw_path: image.raw_path.clone(),
            source,
            protocol,
        });
    }

    #[cfg(feature = "native-images")]
    fn rebuild_native_image(&mut self) {
        let (Some(cache), Some(picker), Some(size)) = (
            self.native_image.as_ref(),
            self.native_picker.as_ref(),
            ui::native_image_size(self),
        ) else {
            return;
        };
        let source = cache.source.clone();
        let protocol = match picker.new_protocol(source, size, Resize::Fit(None)) {
            Ok(protocol) => protocol,
            Err(error) => {
                self.native_image = None;
                self.status_msg = Some(format!("图片重建失败，回退 PathOnly: {error}"));
                return;
            }
        };
        if let Some(cache) = self.native_image.as_mut() {
            cache.protocol = protocol;
        }
    }

    /// 绑定 Markdown 路径；是否建立 watcher 由 `watch_enabled` 独立决定。
    pub fn attach_file(&mut self, path: PathBuf) {
        #[cfg(feature = "native-images")]
        {
            self.native_image = None;
        }
        self.source_path = Some(path.clone());
        self.watcher = self
            .watch_enabled
            .then(|| FileWatcher::watch(path));
        self.rerender();
    }

    /// 进入目录浏览器。
    pub fn enter_browse(&mut self, dir: &Path) {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        self.browse_dir = dir;
        self.browse_entries = browser::scan(&self.browse_dir);
        self.browse_sel = 0;
        self.mode = Mode::Browse;
        self.status_msg = None;
    }

    fn refresh_browse(&mut self) {
        self.browse_entries = browser::scan(&self.browse_dir);
        let last = self.browse_entries.len().saturating_sub(1);
        self.browse_sel = min(self.browse_sel, last);
    }

    /// 打开浏览器当前选中项：目录则进入，文件则阅读。
    pub fn open_selected(&mut self) {
        let Some(entry) = self.browse_entries.get(self.browse_sel).cloned() else {
            return;
        };
        if entry.is_dir {
            self.enter_browse(&entry.path);
        } else {
            self.came_from_browse = true;
            self.open_file(&entry.path);
        }
    }

    /// 打开 Markdown 文件进入阅读；失败时保持当前模式并提示。
    pub fn open_file(&mut self, path: &Path) {
        match std::fs::read_to_string(path) {
            Ok(src) => {
                self.source = src;
                self.title = path.display().to_string();
                self.source_path = Some(path.to_path_buf());
                self.image_panel = ImagePanel::Closed;
                #[cfg(feature = "native-images")]
                {
                    self.native_image = None;
                }
                if self.watch_enabled {
                    self.watcher = Some(FileWatcher::watch(path.to_path_buf()));
                } else {
                    self.watcher = None;
                }
                self.folds.clear();
                self.rerender();
                self.scroll = 0;
                self.toc_selected = 0;
                self.search_input.clear();
                self.search_query = None;
                self.matches.clear();
                self.match_idx = None;
                self.status_msg = None;
                self.mode = Mode::Read;
            }
            Err(e) => {
                self.status_msg = Some(format!("无法打开 {}: {e}", path.display()));
            }
        }
    }

    /// 启动时按配置一次性应用版式（钳制到合法范围）。
    pub fn configure(&mut self, safe_zone: u8, line_gap: u8, para_gap: u8, fold_default: bool) {
        self.safe_zone = safe_zone.min(6);
        self.line_gap = line_gap.min(1);
        self.para_gap = para_gap.min(2);
        self.fold_default = fold_default;
        self.rerender();
    }

    /// 恢复默认设置：dark 主题、安全区 2 列、紧凑行距、标准段距。
    fn restore_defaults(&mut self) {
        self.theme = crate::theme::DARK;
        self.renderer = Renderer::new(self.theme);
        self.safe_zone = 2;
        self.line_gap = 0;
        self.para_gap = 1;
        self.fold_default = false;
        self.status_msg = Some("已恢复默认设置".to_string());
    }

    /// 从磁盘重读当前文件（live reload）；保持滚动位置并刷新搜索命中。
    pub fn reload_from_disk(&mut self) {
        let Some(path) = self.source_path.clone() else { return };
        match std::fs::read_to_string(&path) {
            Ok(src) => {
                self.source = src;
                #[cfg(feature = "native-images")]
                {
                    self.native_image = None;
                }
                self.rerender();
                self.toc_selected = self.toc_selected.min(self.doc.toc.len().saturating_sub(1));
                self.refresh_search_state();
            }
            Err(_) => {
                self.status_msg = Some("文件无法读取（可能已被移动或删除）".to_string());
            }
        }
    }

    /// 把当前设置写入用户配置。
    fn persist_config(&self) {
        config::Config {
            theme: Some(self.theme.name.to_string()),
            safe_zone: Some(self.safe_zone),
            line_gap: Some(self.line_gap),
            para_gap: Some(self.para_gap),
            fold_default: Some(self.fold_default),
        }
        .save();
    }

    /// 按当前宽度与版式设置重渲染；保留可识别的折叠、搜索和图片面板状态。
    fn rerender(&mut self) {
        let inner = self.inner_width();
        let old_folds: Vec<(String, bool)> = self
            .doc
            .collapsibles
            .iter()
            .zip(&self.folds)
            .map(|(collapsible, folded)| (collapsible.title.clone(), *folded))
            .collect();
        let selected_image = match &self.image_panel {
            ImagePanel::Closed => None,
            ImagePanel::PathOnly { image } => Some((image.id, image.raw_path.clone())),
        };
        let mut doc = self
            .renderer
            .render_with(&self.source, inner, self.render_style());
        doc.resolve_image_paths(self.source_path.as_deref());
        self.doc = doc;
        let mut used = vec![false; old_folds.len()];
        self.folds = self
            .doc
            .collapsibles
            .iter()
            .map(|collapsible| {
                old_folds
                    .iter()
                    .enumerate()
                    .find(|(idx, (title, _))| !used[*idx] && title == &collapsible.title)
                    .map(|(idx, (_, folded))| {
                        used[idx] = true;
                        *folded
                    })
                    .unwrap_or(self.fold_default)
            })
            .collect();
        self.image_panel = selected_image
            .and_then(|(id, raw_path)| {
                self.doc
                    .images
                    .iter()
                    .find(|image| image.id == id && image.raw_path == raw_path)
                    .cloned()
            })
            .map_or(ImagePanel::Closed, |image| ImagePanel::PathOnly { image });
        #[cfg(feature = "native-images")]
        {
            let keep_native = self.native_image.as_ref().is_some_and(|cache| {
                matches!(&self.image_panel, ImagePanel::PathOnly { image }
                    if image.id == cache.image_id && image.raw_path == cache.raw_path)
            });
            if keep_native {
                self.rebuild_native_image();
            } else {
                self.native_image = None;
            }
        }
        self.scroll = min(self.scroll, self.max_scroll());
    }

    /// 重渲染并锚定画面：尽量让原视口顶行文本仍出现在同一滚动位置。
    fn rerender_anchored(&mut self) {
        let old_count = self.doc.lines.len().max(1);
        let old_scroll = self.scroll;
        let anchor = self.doc.lines.get(old_scroll).map(line_text);
        self.rerender();
        let new_scroll = match anchor {
            Some(text) => {
                let from = old_scroll.saturating_sub(64);
                (from..self.doc.lines.len())
                    .find(|&i| line_text(&self.doc.lines[i]) == text)
                    .unwrap_or_else(|| old_scroll * self.doc.lines.len().max(1) / old_count)
            }
            None => old_scroll,
        };
        self.scroll = min(new_scroll, self.max_scroll());
    }

    fn render_style(&self) -> crate::markdown::RenderStyle {
        crate::markdown::RenderStyle {
            safe_zone: self.safe_zone,
            line_gap: self.line_gap,
            para_gap: self.para_gap,
        }
    }

    fn inner_width(&self) -> u16 {
        ui::document_width(self.render_width, self)
    }

    /// 渲染一个待处理块（大文件懒渲染）。
    fn render_one_chunk(&mut self) -> bool {
        let w = self.inner_width();
        let style = self.render_style();
        let rendered = self.renderer.render_next_chunk(&mut self.doc, w, style);
        if rendered {
            self.doc.resolve_image_paths(self.source_path.as_deref());
        }
        rendered
    }

    /// 保证视口附近已渲染到足够行数（懒渲染按需推进）。
    pub fn ensure_rendered(&mut self, need_lines: usize) {
        let mut guard = 0;
        while self.doc.lines.len() < need_lines && guard < 256 {
            guard += 1;
            if !self.render_one_chunk() {
                break;
            }
        }
    }

    fn max_scroll(&self) -> usize {
        self.doc.line_count().saturating_sub(self.view_height.max(1))
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = min(self.scroll.saturating_add(n), self.max_scroll());
        self.skip_hidden(true);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
        self.skip_hidden(false);
    }

    /// 滚动落在已折叠块的隐藏区间内时跳到其边缘：
    /// 向下跳到块结束之后、向上跳到 summary 行，保证滚动始终可见。
    fn skip_hidden(&mut self, down: bool) {
        loop {
            let hit = self.doc.collapsibles.iter().enumerate().find(|&(i, c)| {
                self.folds.get(i).copied().unwrap_or(false)
                    && c.end > c.line
                    && c.line < self.scroll
                    && self.scroll < c.end
            });
            let Some((_, c)) = hit else { return };
            self.scroll = if down {
                c.end.min(self.max_scroll())
            } else {
                c.line
            };
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        // G 键直达真实底部：把剩余块全部渲染
        while self.render_one_chunk() {}
        self.scroll = self.max_scroll();
    }

    /// 跳转到指定行：顶对齐（大纲跳转）。
    pub fn goto_line_top(&mut self, line: usize) {
        self.scroll = min(line, self.max_scroll());
    }

    /// 跳转到指定行：尽量垂直居中（搜索命中）。
    pub fn goto_line_center(&mut self, line: usize) {
        self.scroll = min(line.saturating_sub(self.view_height / 2), self.max_scroll());
    }

    /// 阅读进度百分比：以视口底部所在位置计算，封顶 100%。
    /// 懒渲染未完成时按渲染进度折算，且不给满值。
    pub fn progress_percent(&self) -> usize {
        let total = self.doc.line_count();
        if total == 0 {
            return 100;
        }
        if !self.doc.fully_rendered() {
            let rendered_ratio = self
                .doc
                .rendered_source_lines
                .checked_mul(100)
                .map(|n| n / self.doc.total_source_lines.max(1))
                .unwrap_or(100);
            let view_ratio = ((self.scroll + self.view_height) * 100 / total).min(100);
            return (rendered_ratio.min(100) * view_ratio.min(100) / 100).min(99);
        }
        let bottom = min(total, self.scroll.saturating_add(self.view_height));
        (bottom * 100 / total).min(100)
    }

    pub fn on_resize(&mut self, width: u16, height: u16) {
        let next_view_height = (height as usize).saturating_sub(1);
        let height_changed = next_view_height != self.view_height;
        self.view_height = next_view_height;
        if width != self.render_width {
            self.render_width = width;
            self.rerender();
            self.toc_selected = self.toc_selected.min(self.doc.toc.len().saturating_sub(1));
            self.refresh_search_state();
        } else if height_changed {
            #[cfg(feature = "native-images")]
            self.rebuild_native_image();
        }
        self.scroll = min(self.scroll, self.max_scroll());
    }

    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp if self.mode == Mode::Read => self.scroll_up(3),
            MouseEventKind::ScrollDown if self.mode == Mode::Read => self.scroll_down(3),
            MouseEventKind::Down(MouseButton::Left)
                if self.mode == Mode::Read
                    && mouse.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.open_image_at(mouse.column, mouse.row);
            }
            _ => {}
        }
    }

    fn open_image_at(&mut self, column: u16, row: u16) {
        let Some(hit) = ui::image_hit_at(self, column, row) else {
            return;
        };
        let Some(image) = self.doc.images.get(hit.image).cloned() else {
            return;
        };
        if !image.is_local {
            return;
        }
        let was_closed = matches!(self.image_panel, ImagePanel::Closed);
        #[cfg(feature = "native-images")]
        {
            self.native_image = None;
        }
        self.image_panel = ImagePanel::PathOnly {
            image: image.clone(),
        };
        if was_closed {
            self.rerender_anchored();
            self.refresh_search_state();
        }
        #[cfg(feature = "native-images")]
        self.try_load_native_image(&image);
    }

    fn close_image_panel(&mut self) -> bool {
        if matches!(self.image_panel, ImagePanel::Closed) {
            return false;
        }
        self.image_panel = ImagePanel::Closed;
        #[cfg(feature = "native-images")]
        {
            self.native_image = None;
        }
        self.rerender_anchored();
        self.refresh_search_state();
        true
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.mode {
            Mode::Read => self.on_key_read(key, ctrl),
            Mode::Toc => self.on_key_toc(key),
            Mode::SearchInput => self.on_key_search_input(key, ctrl),
            Mode::Settings => self.on_key_settings(key),
            Mode::ZoneInput => self.on_key_zone_input(key),
            Mode::Browse => self.on_key_browse(key),
        }
    }

    fn on_key_read(&mut self, key: KeyEvent, ctrl: bool) {
        self.status_msg = None;
        let half = (self.view_height / 2).max(1);
        let page = self.view_height.max(1);
        match key.code {
            KeyCode::Char('q' | 'Q') => self.quit = true,
            KeyCode::Esc => {
                if self.close_image_panel() {
                    return;
                }
                if self.came_from_browse {
                    let dir = self.browse_dir.clone();
                    self.enter_browse(&dir);
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Char('t') => {
                if !self.doc.toc.is_empty() {
                    self.mode = Mode::Toc;
                    self.rerender_anchored();
                    self.refresh_search_state();
                } else {
                    self.status_msg = Some("本文档没有大纲".to_string());
                }
            }
            KeyCode::Char('/') => {
                self.search_input.clear();
                self.mode = Mode::SearchInput;
            }
            KeyCode::Char('s') => {
                self.mode = Mode::Settings;
                self.rerender_anchored();
                self.refresh_search_state();
            }
            KeyCode::Char('o') if ctrl => self.toggle_all_folds(),
            KeyCode::Char('n') => self.jump_match(1),
            KeyCode::Char('N') => self.jump_match(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_down(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_up(1),
            KeyCode::Char('d') if ctrl => self.scroll_down(half),
            KeyCode::Char('u') if ctrl => self.scroll_up(half),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll_down(page),
            KeyCode::PageUp => self.scroll_up(page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll_to_top(),
            KeyCode::End | KeyCode::Char('G') => self.scroll_to_bottom(),
            _ => {}
        }
    }

    fn on_key_toc(&mut self, key: KeyEvent) {
        let last = self.doc.toc.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('q' | 'Q') => self.quit = true,
            KeyCode::Esc | KeyCode::Char('t') => {
                self.mode = Mode::Read;
                self.rerender_anchored();
                self.refresh_search_state();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.toc_selected = min(self.toc_selected + 1, last);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.toc_selected = self.toc_selected.saturating_sub(1);
            }
            KeyCode::Home | KeyCode::Char('g') => self.toc_selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.toc_selected = last,
            KeyCode::Enter => {
                let info = self
                    .doc
                    .toc
                    .get(self.toc_selected)
                    .map(|e| (e.chunk, e.line));
                if let Some((chunk, line)) = info {
                    // 目标块尚未渲染：先渲染到该块。
                    if line == UNRESOLVED_LINE {
                        while self.doc.rendered_chunks <= chunk && self.render_one_chunk() {}
                    }
                    self.mode = Mode::Read;
                    self.rerender();
                    while self.doc.rendered_chunks <= chunk && self.render_one_chunk() {}
                    self.refresh_search_state();
                    if let Some(target) = self
                        .doc
                        .toc
                        .get(self.toc_selected)
                        .map(|e| e.line)
                        .filter(|l| *l != UNRESOLVED_LINE)
                    {
                        self.goto_line_top(target);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_key_browse(&mut self, key: KeyEvent) {
        let last = self.browse_entries.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('q' | 'Q') => self.quit = true,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                if let Some(parent) = self.browse_dir.parent().map(Path::to_path_buf) {
                    self.enter_browse(&parent);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.browse_sel = min(self.browse_sel + 1, last);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.browse_sel = self.browse_sel.saturating_sub(1);
            }
            KeyCode::Home | KeyCode::Char('g') => self.browse_sel = 0,
            KeyCode::End | KeyCode::Char('G') => self.browse_sel = last,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected(),
            KeyCode::Char('r') => self.refresh_browse(),
            _ => {}
        }
    }

    fn on_key_settings(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q' | 'Q') => self.quit = true,
            KeyCode::Esc | KeyCode::Char('s') => {
                self.mode = Mode::Read;
                self.rerender_anchored();
                self.refresh_search_state();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.settings_sel = min(self.settings_sel + 1, 5);
            }
            KeyCode::Up | KeyCode::Char('k') => self.settings_sel = self.settings_sel.saturating_sub(1),
            KeyCode::Right | KeyCode::Left => {
                self.adjust_setting(key.code == KeyCode::Left);
            }
            KeyCode::Enter => {
                if self.settings_sel == 1 {
                    self.zone_input.clear();
                    self.mode = Mode::ZoneInput;
                } else {
                    self.adjust_setting(false);
                }
            }
            _ => {}
        }
    }

    /// ZoneInput 模式：输入 0-6 的安全区列数。
    fn on_key_zone_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q' | 'Q') => self.quit = true,
            KeyCode::Esc => {
                self.zone_input.clear();
                self.mode = Mode::Settings;
            }
            KeyCode::Enter => {
                let current = self.safe_zone;
                let n = self.zone_input.parse::<u8>().unwrap_or(current).min(6);
                self.safe_zone = n;
                self.zone_input.clear();
                self.mode = Mode::Settings;
                self.rerender_anchored();
                self.refresh_search_state();
                self.persist_config();
            }
            KeyCode::Backspace => {
                self.zone_input.pop();
            }
            KeyCode::Char(c) if ('0'..='6').contains(&c) => self.zone_input.push(c),
            _ => {}
        }
    }

    /// 调整设置项：Enter/→ 取下一值，← 取上一值；变更写入用户配置。
    fn adjust_setting(&mut self, backward: bool) {
        match self.settings_sel {
            0 => {
                let all = crate::theme::ALL;
                let idx = all.iter().position(|t| *t == self.theme).unwrap_or(0);
                let next = if backward {
                    (idx + all.len() - 1) % all.len()
                } else {
                    (idx + 1) % all.len()
                };
                self.theme = all[next];
                self.renderer = Renderer::new(self.theme);
            }
            1 => {
                let n = self.safe_zone as isize + if backward { -1 } else { 1 };
                self.safe_zone = n.rem_euclid(3) as u8;
            }
            2 => self.line_gap = 1 - self.line_gap.min(1),
            3 => {
                let n = self.para_gap as isize + if backward { -1 } else { 1 };
                self.para_gap = n.rem_euclid(3) as u8;
            }
            4 => {
                // 仅切换显示默认，不触发重排：立即应用到当前文档
                self.fold_default = !self.fold_default;
                self.folds = vec![self.fold_default; self.doc.collapsibles.len()];
            }
            _ => self.restore_defaults(),
        }
        if self.settings_sel != 4 {
            self.rerender_anchored();
            self.refresh_search_state();
        }
        self.persist_config();
    }

    fn on_key_search_input(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Esc => {
                self.search_input.clear();
                self.mode = Mode::Read;
            }
            KeyCode::Enter => self.commit_search(),
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Char('u') if ctrl => self.search_input.clear(),
            KeyCode::Char(c) => self.search_input.push(c),
            _ => {}
        }
    }

    fn commit_search(&mut self) {
        self.mode = Mode::Read;
        let q = self.search_input.trim().to_string();
        if q.is_empty() {
            self.search_query = None;
            self.matches.clear();
            self.match_idx = None;
            return;
        }
        self.search_query = Some(q.clone());
        self.refresh_matches();
        self.match_idx = None;
        if self.matches.is_empty() {
            self.status_msg = Some(format!("未找到 “{q}”"));
        } else {
            self.jump_match(1);
        }
    }

    /// 按当前 query 重算命中行（大小写不敏感、按渲染后行文本匹配）。
    pub fn refresh_matches(&mut self) {
        self.matches = match &self.search_query {
            Some(q) => {
                let ql = q.to_lowercase();
                self.doc
                    .lines
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| line_text(l).to_lowercase().contains(&ql))
                    .map(|(i, _)| i)
                    .collect()
            }
            None => Vec::new(),
        };
    }

    fn refresh_search_state(&mut self) {
        self.match_idx = None;
        if self.search_query.is_some() {
            self.refresh_matches();
        }
    }

    /// 在命中项间跳转：dir 为 1 下一个、-1 上一个，循环滚动。
    fn jump_match(&mut self, dir: isize) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len();
        let idx = match self.match_idx {
            Some(i) => (i as isize + dir).rem_euclid(len as isize) as usize,
            None => 0,
        };
        self.match_idx = Some(idx);
        let line = self.matches[idx];
        self.goto_line_center(line);
    }

    /// 在全部展开与全部收起之间切换 `<details>` 折叠块（Read 模式 `Ctrl+O`）。
    fn toggle_all_folds(&mut self) {
        // 先完成懒渲染，确保文档中的所有折叠块都纳入操作范围。
        while self.render_one_chunk() {}
        let count = self.doc.collapsibles.len();
        if count == 0 {
            self.status_msg = Some("文档中没有可折叠块".to_string());
            return;
        }
        let should_fold = self.folds.iter().take(count).all(|folded| !*folded);
        self.folds = vec![should_fold; count];
    }
}

#[cfg(feature = "native-images")]
fn picker_supports_native(picker: &Picker) -> bool {
    let has_pixel_size = picker.capabilities().iter().any(|cap| {
        matches!(cap, Capability::CellSize(Some((width, height))) if *width > 0 && *height > 0)
    });
    if !has_pixel_size {
        return false;
    }
    match picker.protocol_type() {
        ProtocolType::Kitty => picker.capabilities().contains(&Capability::Kitty),
        ProtocolType::Sixel => picker.capabilities().contains(&Capability::Sixel),
        ProtocolType::Iterm2 => true,
        ProtocolType::Halfblocks => false,
    }
}

fn line_text(l: &Line<'_>) -> String {
    l.spans.iter().map(|s| s.content.as_ref()).collect()
}

pub fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let (w, h) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    app.on_resize(w, h);
    loop {
        // 懒渲染：保证视口下方有一屏余量
        app.ensure_rendered(app.scroll + app.view_height.max(1) + 64);
        terminal.draw(|f| ui::draw(f, app))?;
        if app.watcher.as_mut().is_some_and(|w| w.changed()) {
            app.reload_from_disk();
            continue; // 立即重绘新内容
        }
        if ratatui::crossterm::event::poll(Duration::from_millis(100))? {
            match ratatui::crossterm::event::read()? {
                Event::Key(key) => app.on_key(key),
                Event::Mouse(mouse) => app.on_mouse(mouse),
                Event::Resize(w, h) => app.on_resize(w, h),
                _ => {}
            }
        }
        if app.quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_lines(n: usize) -> App {
        // 每行作为独立段落（空行分隔），行数可预期：2n
        let src: Vec<String> = (0..n).map(|i| format!("line {i}")).collect();
        App::new(&src.join("\n\n"), "test.md".into(), crate::theme::DARK)
    }

    #[test]
    fn scroll_clamps_at_bounds() {
        let mut app = app_with_lines(100);
        app.view_height = 10;
        app.scroll_down(500);
        assert_eq!(app.scroll, 190);
        app.scroll_up(500);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn scroll_skips_hidden_fold_regions() {
        let mut app = fold_fixture();
        assert_eq!(app.doc.collapsibles.len(), 2);
        app.folds[0] = true;
        app.view_height = 2; // 让 max_scroll 覆盖到块结束
        let c = app.doc.collapsibles[0].clone();
        // 从 summary 上方向下滚动进入隐藏区间 → 应跳到块结束之后
        app.scroll = c.line.saturating_sub(1);
        app.scroll_down(3);
        assert_eq!(app.scroll, c.end, "向下应跳过隐藏区间: {}", c.end);
        // 从块结束之后向上滚动进入隐藏区间 → 应跳回 summary 行
        app.scroll = c.end;
        app.scroll_up(1);
        assert_eq!(app.scroll, c.line, "向上应回到 summary 行");
    }

    #[test]
    fn keys_scroll_half_page_and_page() {
        let mut app = app_with_lines(200);
        app.view_height = 20;
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.scroll, 10);
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.scroll, 0);
        app.on_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.scroll, 20);
        app.on_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn g_home_top_and_g_end_bottom() {
        let mut app = app_with_lines(100);
        app.view_height = 10;
        app.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        assert_eq!(app.scroll, 190);
        app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
        app.scroll_to_bottom();
        app.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
        app.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.scroll, 190);
    }

    #[test]
    fn q_ctrl_c_esc_quit() {
        for (code, mods) in [
            (KeyCode::Char('q'), KeyModifiers::NONE),
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
            (KeyCode::Esc, KeyModifiers::NONE),
        ] {
            let mut app = app_with_lines(10);
            app.on_key(KeyEvent::new(code, mods));
            assert!(app.quit, "{code:?} 应退出");
        }
    }

    #[test]
    fn progress_percent_boundaries() {
        // 50 段 → 100 行；视口 10 行时底部在第 10 行 = 10%
        let mut app = app_with_lines(50);
        app.view_height = 10;
        assert_eq!(app.progress_percent(), 10);
        app.scroll_to_bottom();
        assert_eq!(app.progress_percent(), 100);
    }

    #[test]
    fn resize_clamps_scroll_and_sets_view_height() {
        let mut app = app_with_lines(100);
        app.scroll_down(50);
        app.on_resize(80, 30);
        assert_eq!(app.view_height, 29);
        assert_eq!(app.scroll, 50);
        app.scroll = 200;
        app.on_resize(80, 30);
        assert_eq!(app.scroll, 200 - 29);
    }

    #[test]
    fn short_document_never_panics() {
        let mut app = app_with_lines(3);
        app.view_height = 10;
        app.scroll_down(10);
        assert_eq!(app.scroll, 0);
        assert_eq!(app.progress_percent(), 100);
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    #[test]
    fn toc_open_navigate_and_jump() {
        let src = "# 甲\n\n段落\n\n## 乙\n\n段落\n\n### 丙\n\n段落\n";
        let mut app = App::new(src, "t.md".into(), crate::theme::DARK);
        assert_eq!(app.doc.toc.len(), 3);
        app.view_height = 5;
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Toc);
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.toc_selected, 2);
        app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.toc_selected, 1);
        let target = app.doc.toc[1].line;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Read);
        assert_eq!(app.scroll, target.min(app.max_scroll()));
    }

    #[test]
    fn toc_empty_document_shows_message() {
        let mut app = app_with_lines(3);
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Read);
        assert_eq!(app.status_msg.as_deref(), Some("本文档没有大纲"));
    }

    #[test]
    fn toc_selection_clamps_at_bounds() {
        let src = "# 甲\n\n## 乙\n";
        let mut app = App::new(src, "t.md".into(), crate::theme::DARK);
        app.mode = Mode::Toc;
        for _ in 0..10 {
            app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        assert_eq!(app.toc_selected, app.doc.toc.len() - 1);
        for _ in 0..10 {
            app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        }
        assert_eq!(app.toc_selected, 0);
    }

    #[test]
    fn search_flow_finds_and_jumps() {
        let src = "alpha one\n\nbeta two\n\nAlpha three\n\ngamma alpha\n\ndelta\n";
        let mut app = App::new(src, "s.md".into(), crate::theme::DARK);
        app.view_height = 10;
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::SearchInput);
        type_str(&mut app, "alpha");
        assert_eq!(app.search_input, "alpha");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Read);
        assert_eq!(app.search_query.as_deref(), Some("alpha"));
        assert_eq!(app.matches.len(), 3, "大小写不敏感应命中 3 处");
        assert_eq!(app.match_idx, Some(0));
        // 第一个命中行在视口内
        let first = app.matches[0];
        assert!(app.scroll <= first && first < app.scroll + app.view_height);
        // n 循环
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.match_idx, Some(1));
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.match_idx, Some(2));
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.match_idx, Some(0), "n 应循环回第一个");
        app.on_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::NONE));
        assert_eq!(app.match_idx, Some(2), "N 应回到上一个");
    }

    #[test]
    fn search_no_match_sets_status() {
        let mut app = app_with_lines(5);
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        type_str(&mut app, "zzz");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Read);
        assert!(app.matches.is_empty());
        assert_eq!(app.status_msg.as_deref(), Some("未找到 “zzz”"));
        // n/N 空命中不 panic
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::NONE));
    }

    #[test]
    fn search_esc_cancels_input() {
        let mut app = app_with_lines(5);
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        type_str(&mut app, "line");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Read);
        assert!(app.search_input.is_empty());
        assert!(app.search_query.is_none());
    }

    #[test]
    fn search_backspace_edits_query() {
        let mut app = app_with_lines(5);
        app.mode = Mode::SearchInput;
        type_str(&mut app, "abc");
        app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.search_input, "ab");
    }

    #[test]
    fn resize_keeps_active_search_working() {
        let src = "关键词甲在这段\n\n其他内容\n\n再次提及关键词甲\n";
        let mut app = App::new(src, "r.md".into(), crate::theme::DARK);
        app.mode = Mode::SearchInput;
        type_str(&mut app, "关键词");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.matches.len(), 2);
        app.on_resize(120, 30);
        assert_eq!(app.matches.len(), 2, "重渲染后命中应保留");
        assert!(app.match_idx.is_none(), "行号已变化应重置当前位置");
    }

    #[test]
    fn status_msg_cleared_by_next_read_key() {
        let mut app = app_with_lines(5);
        app.status_msg = Some("未找到 “x”".into());
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(app.status_msg.is_none());
    }

    #[test]
    fn settings_open_close_and_search_unaffected() {
        let mut app = app_with_lines(5);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Settings);
        // 设置面板内按 / 不会进入搜索，两种输入互不冲突
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Settings);
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Read);
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::SearchInput);
    }

    #[test]
    fn settings_cycle_theme() {
        let all = crate::theme::ALL;
        let expected_lines = app_with_lines(5).doc.line_count();
        let mut app = app_with_lines(5);
        app.mode = Mode::Settings;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.theme, all[1]);
        assert_eq!(app.doc.line_count(), expected_lines, "重渲染行数应一致");
        // ← 回到第一个
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.theme, all[0]);
        // → 循环到末尾再回绕
        for _ in 0..all.len() - 1 {
            app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        }
        assert_eq!(app.theme, all[all.len() - 1]);
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.theme, all[0]);
    }

    #[test]
    fn light_theme_is_removed() {
        assert!(crate::theme::Theme::by_name(Some("light")).is_err());
    }

    #[test]
    fn settings_adjust_safe_zone_rerenders() {
        let expected_lines = app_with_lines(5).doc.line_count();
        let mut app = app_with_lines(5);
        assert_eq!(app.safe_zone, 2, "默认 2 列安全区");
        app.mode = Mode::Settings;
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.safe_zone, 0);
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.safe_zone, 1);
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.safe_zone, 0, "← 反向循环");
        assert_eq!(app.doc.line_count(), expected_lines);
    }

    #[test]
    fn reload_from_disk_picks_up_changes() {
        let dir = std::env::temp_dir().join(format!("red-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.md");
        std::fs::write(&path, "旧内容\n").unwrap();
        let mut app = App::new(
            &std::fs::read_to_string(&path).unwrap(),
            path.display().to_string(),
            crate::theme::DARK,
        );
        app.attach_file(path.clone());
        std::fs::write(&path, "新内容出现\n").unwrap();
        app.reload_from_disk();
        let joined: String = app.doc.lines.iter().map(|l| line_text(l)).collect();
        assert!(joined.contains("新内容出现"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reload_without_attached_file_is_noop() {
        let mut app = app_with_lines(3);
        app.reload_from_disk();
        assert!(app.status_msg.is_none());
    }

    #[test]
    fn configure_clamps_layout_settings() {
        let mut app = app_with_lines(5);
        app.configure(9, 7, 8, true);
        assert_eq!(app.safe_zone, 6);
        assert_eq!(app.line_gap, 1);
        assert_eq!(app.para_gap, 2);
        assert!(app.fold_default);
        app.configure(0, 0, 0, false);
        assert_eq!(app.safe_zone, 0);
        assert_eq!(app.line_gap, 0);
        assert_eq!(app.para_gap, 0);
        assert!(!app.fold_default);
    }

    #[test]
    fn settings_selection_clamps() {
        let mut app = app_with_lines(5);
        app.mode = Mode::Settings;
        for _ in 0..10 {
            app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        assert_eq!(app.settings_sel, 5);
        for _ in 0..10 {
            app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        }
        assert_eq!(app.settings_sel, 0);
    }

    #[test]
    fn settings_gap_and_restore_defaults() {
        let mut app = app_with_lines(5);
        app.mode = Mode::Settings;
        // 行距（第 3 项）：紧凑 → 宽松
        app.settings_sel = 2;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.line_gap, 1);
        // 段距（第 4 项）：标准 → 宽松
        app.settings_sel = 3;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.para_gap, 2);
        // 主题切走以便验证恢复
        app.settings_sel = 0;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.theme, crate::theme::GRUVBOX);
        // 安全区改成 1 以便验证恢复
        app.settings_sel = 1;
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.safe_zone, 0);
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.safe_zone, 1);
        // 折叠默认（第 5 项）：展开 → 收起，且立即应用到当前文档
        app.settings_sel = 4;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.fold_default);
        // 恢复默认（第 6 项）
        app.settings_sel = 5;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.theme, crate::theme::DARK);
        assert_eq!(app.safe_zone, 2);
        assert_eq!(app.line_gap, 0);
        assert_eq!(app.para_gap, 1);
        assert!(!app.fold_default);
        assert_eq!(app.status_msg.as_deref(), Some("已恢复默认设置"));
    }

    #[test]
    fn zone_input_commit_cancel_and_clamp() {
        let mut app = app_with_lines(5);
        app.mode = Mode::Settings;
        app.settings_sel = 1;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::ZoneInput);
        // 输入 "59"：5 追加，9 忽略
        type_str(&mut app, "59");
        assert_eq!(app.zone_input, "5");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Settings);
        assert_eq!(app.safe_zone, 5);
        // 越界钳制到 6；7/9 不是合法数字
        app.settings_sel = 1;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        type_str(&mut app, "9796");
        assert_eq!(app.zone_input, "6", "7/9 不是合法数字");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.safe_zone, 6, "越界应钳制到 6");
        // Esc 取消，非数字忽略
        app.settings_sel = 1;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        type_str(&mut app, "3x");
        assert_eq!(app.zone_input, "3", "非数字应忽略");
        app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(app.zone_input.is_empty());
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Settings);
        assert!(app.zone_input.is_empty());
        assert_eq!(app.safe_zone, 6, "取消不应改动");
        // 空输入 Enter = 取消
        app.settings_sel = 1;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Settings);
        assert_eq!(app.safe_zone, 6);
    }

    #[test]
    fn rerender_anchored_keeps_text_line_at_same_scroll() {
        // 长段落会折行，行距切换后行数变化
        let src = "这是一段很长的中文内容用于折行锚定测试。".repeat(30);
        let mut app = App::new(&src, "a.md".into(), crate::theme::DARK);
        app.view_height = 10;
        app.scroll_down(20);
        let before = app.doc.lines.len();
        let anchor_text: String = app
            .doc
            .lines
            .get(app.scroll)
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .unwrap_or_default();
        assert!(!anchor_text.trim().is_empty());
        // 行距切换改变行数，锚定后仍落在原段落附近；设置侧栏宽度也参与重排。
        app.settings_sel = 2;
        app.mode = Mode::Settings;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.line_gap, 1);
        assert!(app.doc.lines.len() > before, "行距宽松后行数应增加");
        let now_text: String = app
            .doc
            .lines
            .get(app.scroll)
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .unwrap_or_default();
        assert!(
            now_text.contains("折行锚定测试"),
            "重排后仍应落在原段落附近: {now_text:?}"
        );
    }

    fn fold_fixture() -> App {
        let src = "开头\n\n<details>\n<summary>点我展开</summary>\n\n- 列表项\n\n```js\nconsole.log(1);\n```\n\n</details>\n\n结尾可见\n\n<details>\n<summary>第二个</summary>\n\n第二块内容\n\n</details>\n";
        App::new(src, "f.md".into(), crate::theme::DARK)
    }

    #[test]
    fn ctrl_o_toggles_all_details_blocks() {
        let mut app = fold_fixture();
        assert_eq!(app.doc.collapsibles.len(), 2, "两个 details 块");
        assert!(app.folds.iter().all(|f| !*f));

        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(app.folds.iter().all(|f| *f), "第一次 Ctrl+O 应全部收起");
        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(app.folds.iter().all(|f| !*f), "第二次 Ctrl+O 应全部展开");
    }

    #[test]
    fn plain_o_does_not_toggle_details_blocks() {
        let mut app = fold_fixture();

        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));

        assert!(app.folds.iter().all(|f| !*f), "普通 o 不应操作折叠块");
    }

    #[test]
    fn ctrl_o_without_details_shows_status() {
        let mut app = app_with_lines(3);

        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));

        assert_eq!(app.status_msg.as_deref(), Some("文档中没有可折叠块"));
    }

    #[test]
    fn settings_toggle_fold_default_applies_to_document() {
        let mut app = fold_fixture();
        assert_eq!(app.doc.collapsibles.len(), 2);
        assert!(app.folds.iter().all(|f| !*f), "默认展开");
        app.mode = Mode::Settings;
        app.settings_sel = 4;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.fold_default);
        assert!(app.folds.iter().all(|f| *f), "切换后当前文档立即收起");
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert!(!app.fold_default);
        assert!(app.folds.iter().all(|f| !*f), "切回展开");
    }

    fn image_fixture() -> App {
        App::new("![图](assets/图.png)\n\n普通正文\n", "image.md".into(), crate::theme::DARK)
    }

    #[cfg(feature = "native-images")]
    #[test]
    fn halfblocks_never_count_as_native_capability_without_querying() {
        assert!(!picker_supports_native(&Picker::halfblocks()));
    }

    #[test]
    fn ctrl_left_opens_path_only_panel_but_plain_left_does_not() {
        let mut app = image_fixture();
        let hit = app.doc.image_hits[0].clone();
        let x = u16::from(app.safe_zone) + hit.start_col as u16;
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: hit.line as u16,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(app.image_panel, ImagePanel::Closed));
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: hit.line as u16,
            modifiers: KeyModifiers::CONTROL,
        });
        assert!(matches!(app.image_panel, ImagePanel::PathOnly { .. }));
        assert!(app.inner_width() < app.render_width);
    }

    #[test]
    fn image_panel_esc_closes_before_read_exit() {
        let mut app = image_fixture();
        let hit = app.doc.image_hits[0].clone();
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: u16::from(app.safe_zone) + hit.start_col as u16,
            row: hit.line as u16,
            modifiers: KeyModifiers::CONTROL,
        });
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.image_panel, ImagePanel::Closed));
        assert!(!app.quit);
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.quit);
    }

    #[test]
    fn mouse_wheel_scrolls_reading_mode() {
        let mut app = app_with_lines(100);
        app.view_height = 5;
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.scroll, 3);
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn no_watch_still_resolves_relative_image_path() {
        let dir = std::env::temp_dir().join(format!("red-image-path-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        let path = dir.join("doc.md");
        std::fs::write(&path, "![图](assets/a.png)\n").unwrap();
        let mut app = App::new(
            &std::fs::read_to_string(&path).unwrap(),
            path.display().to_string(),
            crate::theme::DARK,
        );
        app.watch_enabled = false;
        app.attach_file(path.clone());
        assert!(app.watcher.is_none());
        assert_eq!(app.source_path.as_deref(), Some(path.as_path()));
        assert_eq!(
            app.doc.images[0].resolved_path,
            Some(dir.join("assets/a.png"))
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn browse_fixture() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("red-browse-{}-{n}", std::process::id()));
        let sub = dir.join("notes");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("b.md"), "# B 标题\n\n正文乙\n").unwrap();
        std::fs::write(dir.join("a.markdown"), "# 甲标题\n\n正文段落\n").unwrap();
        std::fs::write(dir.join("skip.txt"), "x").unwrap();
        dir
    }

    #[test]
    fn browse_navigate_and_open_file() {
        let dir = browse_fixture();
        let mut app = app_with_lines(1);
        app.enter_browse(&dir);
        assert_eq!(app.mode, Mode::Browse);
        // 条目：..、notes、a.markdown、b.md
        assert_eq!(app.browse_entries.len(), 4);
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.browse_sel, 2);
        // 打开 a.markdown
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Read);
        assert!(app.came_from_browse);
        let joined: String = app.doc.lines.iter().map(|l| line_text(l)).collect();
        assert!(joined.contains("甲标题"), "应渲染 a.markdown: {joined}");
        assert!(joined.contains("正文段落"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn browse_open_dir_descends_and_esc_returns() {
        let dir = browse_fixture();
        let mut app = app_with_lines(1);
        app.enter_browse(&dir);
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)); // notes
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Browse);
        assert!(app.browse_dir.ends_with("notes"));
        assert_eq!(app.browse_entries.len(), 1, "空目录只有 ..");
        // Esc 回到上级
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.browse_dir == dir.canonicalize().unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn esc_in_read_returns_to_browse_when_opened_from_it() {
        let dir = browse_fixture();
        let mut app = app_with_lines(1);
        app.enter_browse(&dir);
        app.browse_sel = 3; // b.md
        app.open_selected();
        assert_eq!(app.mode, Mode::Read);
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Browse);
        // 直接构造（非浏览器来源）的 Esc 仍然退出
        let mut plain = app_with_lines(3);
        plain.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(plain.quit);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_file_failure_keeps_mode_and_reports() {
        let mut app = app_with_lines(1);
        app.enter_browse(&std::env::temp_dir());
        app.browse_entries.push(browser::BrowserEntry {
            name: "ghost.md".into(),
            path: std::env::temp_dir().join(format!("red-no-such-{}.md", std::process::id())),
            is_dir: false,
            size: 0,
        });
        app.browse_sel = app.browse_entries.len() - 1;
        app.open_selected();
        assert_eq!(app.mode, Mode::Browse);
        assert!(app.status_msg.as_deref().unwrap().starts_with("无法打开"));
    }

    #[test]
    fn lazy_document_toc_jump_and_real_bottom() {
        let mut src = String::new();
        for i in 0..21000 {
            if i % 5000 == 0 {
                src.push_str(&format!("# 章 {i}\n\n"));
            }
            src.push_str("这一段是用于撑起大文件的中文内容。\n\n");
        }
        let mut app = App::new(&src, "big.md".into(), crate::theme::DARK);
        assert!(!app.doc.fully_rendered());
        assert_eq!(app.doc.toc.len(), 5);
        // 大纲跳转到尚未渲染的块
        app.mode = Mode::Toc;
        app.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        assert_eq!(app.doc.toc.last().unwrap().line, UNRESOLVED_LINE);
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Read);
        assert_ne!(app.doc.toc.last().unwrap().line, UNRESOLVED_LINE);
        // G 键渲染全部并到达真实底部
        app.view_height = 10;
        app.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        assert!(app.doc.fully_rendered());
        assert_eq!(app.progress_percent(), 100);
    }
}

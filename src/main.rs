mod app;
mod browser;
mod config;
mod highlight;
mod markdown;
mod term;
mod theme;
mod ui;
mod watcher;

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Parser;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::tty::IsTty;

/// red — 终端 Markdown 阅读器
#[derive(Parser, Debug)]
#[command(name = "red", version, about = "TUI Markdown 阅读器")]
struct Cli {
    /// Markdown 文件路径
    file: Option<PathBuf>,
    /// 初始主题 [dark|gruvbox|nord|dracula|catppuccin]
    #[arg(long)]
    theme: Option<String>,
    /// 禁用 live reload
    #[arg(long)]
    no_watch: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // 配置优先级：默认 < 配置文件 < CLI；无效配置项自动忽略
    let cfg = config::Config::load();
    let theme_arg = cli
        .theme
        .or(cfg.theme)
        .filter(|n| theme::Theme::by_name(Some(n)).is_ok());
    let theme = theme::Theme::by_name(theme_arg.as_deref())?.resolved();

    let mut app = match &cli.file {
        // `red -`：读取 stdin，静态渲染输出（不进入 TUI）
        Some(p) if p.as_os_str() == "-" => {
            let source = read_source(p)?;
            return static_render(&source, theme);
        }
        // 目录参数：浏览该目录
        Some(p) if p.is_dir() => {
            let mut app = app::App::new("", p.display().to_string(), theme);
            app.enter_browse(p);
            app
        }
        // 文件参数：直接阅读
        Some(p) => {
            let source = read_source(p)?;
            let mut app = app::App::new(&source, display_title(p), theme);
            app.watch_enabled = !cli.no_watch;
            if p.is_file() {
                app.attach_file(p.clone());
            }
            app
        }
        // 无参数：浏览当前目录
        None => {
            let cwd = std::env::current_dir().context("无法获取当前目录")?;
            let mut app = app::App::new("", cwd.display().to_string(), theme);
            app.enter_browse(&cwd);
            app
        }
    };
    app.configure(
        cfg.safe_zone.unwrap_or(2),
        cfg.line_gap.unwrap_or(0),
        cfg.para_gap.unwrap_or(1),
        cfg.fold_default.unwrap_or(false),
    );
    if cli.no_watch {
        app.watch_enabled = false;
        app.watcher = None;
    }
    run_tui(app)
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = ratatui::crossterm::execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}

fn run_tui(mut app: app::App) -> Result<()> {
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard;
    ratatui::crossterm::execute!(io::stdout(), EnableMouseCapture)?;
    #[cfg(feature = "native-images")]
    app.init_native_images();
    app::run(&mut terminal, &mut app)
}

/// 管道输入的静态渲染：渲染完整文档并以 ANSI 输出到 stdout（不进入 TUI）。
fn static_render(source: &str, theme: theme::Theme) -> Result<()> {
    let renderer = markdown::Renderer::new(theme);
    let width = ratatui::crossterm::terminal::size()
        .map(|(w, _)| w)
        .unwrap_or(80);
    let style = markdown::RenderStyle::default();
    let mut doc = renderer.render_with(source, width, style);
    while renderer.render_next_chunk(&mut doc, width, style) {}
    use std::io::Write;
    let mut out = io::stdout().lock();
    out.write_all(ui::to_ansi(&doc).as_bytes())?;
    out.flush()?;
    Ok(())
}

fn display_title(path: &Path) -> String {
    path.display().to_string()
}

fn read_source(path: &Path) -> Result<String> {
    if path.as_os_str() == "-" {
        if io::stdin().is_tty() {
            bail!("`-` 用于读取管道输入；当前 stdin 是终端，请传入文件路径");
        }
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .context("读取 stdin 失败")?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("无法读取 {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_render_keeps_stdin_branch_without_image_protocol() {
        let renderer = markdown::Renderer::new(theme::DARK);
        let mut doc = renderer.render_with(
            "![alt](local/image.png)\n",
            80,
            markdown::RenderStyle::default(),
        );
        while renderer.render_next_chunk(&mut doc, 80, markdown::RenderStyle::default()) {}
        let output = ui::to_ansi(&doc);
        assert!(output.contains("alt"));
        assert!(output.contains("local/image.png"));
        assert!(!output.contains("red-image://"));
        assert!(doc.images[0].resolved_path.is_none());
    }
}

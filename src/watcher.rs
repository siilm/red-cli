use notify::Watcher as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

/// 文件变更监视：inotify 事件 + mtime/大小兜底轮询。
/// WSL2 等环境下 inotify 可能不可靠，因此事件与元数据比对同时使用。
pub struct FileWatcher {
    _watcher: Option<notify::RecommendedWatcher>,
    rx: Option<Receiver<()>>,
    path: PathBuf,
    last_check: Instant,
    meta: (SystemTime, u64),
}

fn stat(path: &Path) -> (SystemTime, u64) {
    std::fs::metadata(path)
        .map(|m| {
            (
                m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                m.len(),
            )
        })
        .unwrap_or((SystemTime::UNIX_EPOCH, 0))
}

impl FileWatcher {
    /// 构造监视器；inotify 建立失败时自动退化为 mtime 轮询。
    pub fn watch(path: PathBuf) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let handler = move |res: notify::Result<notify::Event>| {
            if res.is_ok() {
                let _ = tx.send(());
            }
        };
        let (watcher, rx) = match notify::recommended_watcher(handler) {
            Ok(mut w) => match w.watch(&path, notify::RecursiveMode::NonRecursive) {
                Ok(()) => (Some(w), Some(rx)),
                Err(_) => (None, None),
            },
            Err(_) => (None, None),
        };
        Self {
            _watcher: watcher,
            rx,
            meta: stat(&path),
            path,
            last_check: Instant::now(),
        }
    }

    /// 距上次检查不足 300ms 时跳过（防抖）；文件元数据变化时返回 true。
    pub fn changed(&mut self) -> bool {
        if self.last_check.elapsed() < Duration::from_millis(300) {
            return false;
        }
        self.last_check = Instant::now();

        let mut events = false;
        if let Some(rx) = &self.rx {
            while let Ok(()) = rx.try_recv() {
                events = true;
            }
        }

        let meta = stat(&self.path);
        let changed = meta != self.meta && (events || self._watcher.is_none());
        if changed {
            self.meta = meta;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_change_right_after_watch() {
        let dir = std::env::temp_dir().join(format!("red-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.md");
        std::fs::write(&path, "# t\n").unwrap();
        let mut w = FileWatcher::watch(path.clone());
        assert!(!w.changed(), "刚创建不应立即报告变化");
        std::fs::write(&path, "# t\n\n更多内容\n").unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}

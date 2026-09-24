use std::fs;
use std::path::{Path, PathBuf};

/// 目录浏览器条目。
#[derive(Debug, Clone)]
pub struct BrowserEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

/// 扫描目录：上级入口（非根目录时）、子目录、Markdown 文件；隐藏项跳过。
/// 目录在前、文件在后，各自按名称排序（不区分大小写）。
pub fn scan(dir: &Path) -> Vec<BrowserEntry> {
    let mut dirs: Vec<BrowserEntry> = Vec::new();
    let mut files: Vec<BrowserEntry> = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            dirs.push(BrowserEntry {
                name,
                path,
                is_dir: true,
                size: 0,
            });
        } else if is_markdown(&name) {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push(BrowserEntry {
                name,
                path,
                is_dir: false,
                size,
            });
        }
    }
    dirs.sort_by_key(|e| e.name.to_lowercase());
    files.sort_by_key(|e| e.name.to_lowercase());
    let mut out = Vec::with_capacity(dirs.len() + files.len() + 1);
    if let Some(parent) = dir.parent() {
        out.push(BrowserEntry {
            name: "..".to_string(),
            path: parent.to_path_buf(),
            is_dir: true,
            size: 0,
        });
    }
    out.extend(dirs);
    out.extend(files);
    out
}

fn is_markdown(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

/// 人类可读的文件大小。
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0usize;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_filters_and_sorts() {
        let dir = std::env::temp_dir().join(format!("red-scan-{}", std::process::id()));
        let sub = dir.join("notes");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join("b.md"), "# B\n").unwrap();
        fs::write(dir.join("a.markdown"), "# A\n").unwrap();
        fs::write(dir.join("skip.txt"), "x").unwrap();
        fs::write(dir.join(".hidden.md"), "x").unwrap();
        let entries = scan(&dir);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["..", "notes", "a.markdown", "b.md"], "{names:?}");
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].path, dir.parent().unwrap());
        assert_eq!(entries[3].size, 4, "b.md 大小");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_root_has_no_parent_entry() {
        let entries = scan(Path::new("/"));
        assert!(entries.iter().all(|e| e.name != ".."));
    }

    #[test]
    fn human_size_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(4), "4 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }
}

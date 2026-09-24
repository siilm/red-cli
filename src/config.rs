use std::fs;
use std::path::{Path, PathBuf};

/// 用户配置：持久化主题与版式设置。
/// 序列化为简洁 TOML，不引入 toml 依赖。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub theme: Option<String>,
    pub safe_zone: Option<u8>,
    pub line_gap: Option<u8>,
    pub para_gap: Option<u8>,
    pub fold_default: Option<bool>,
}

impl Config {
    /// 配置文件路径：$XDG_CONFIG_HOME/red/config.toml，缺省 ~/.config/red/config.toml。
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("red").join("config.toml"))
    }

    /// 从磁盘加载；文件不存在或损坏时返回默认值（不报错）。
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|s| Self::parse(&s))
            .unwrap_or_default()
    }

    pub fn parse(s: &str) -> Self {
        let mut c = Self::default();
        for line in s.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "theme" => {
                    // 只接受已知主题名，损坏/陌生的配置不生效
                    if crate::theme::ALL.iter().any(|t| t.name == v) {
                        c.theme = Some(v.to_string());
                    }
                }
                "safe_zone" => c.safe_zone = v.parse().ok(),
                "line_gap" => c.line_gap = v.parse().ok(),
                "para_gap" => c.para_gap = v.parse().ok(),
                "fold_default" => c.fold_default = v.parse().ok(),
                _ => {}
            }
        }
        c
    }

    pub fn to_toml(&self) -> String {
        let mut s = String::new();
        if let Some(t) = &self.theme {
            s.push_str(&format!("theme = \"{t}\"\n"));
        }
        if let Some(z) = self.safe_zone {
            s.push_str(&format!("safe_zone = {z}\n"));
        }
        if let Some(g) = self.line_gap {
            s.push_str(&format!("line_gap = {g}\n"));
        }
        if let Some(g) = self.para_gap {
            s.push_str(&format!("para_gap = {g}\n"));
        }
        if let Some(f) = self.fold_default {
            s.push_str(&format!("fold_default = {f}\n"));
        }
        s
    }

    /// 尽力保存；失败静默（终端应用不应因配置写入失败中断阅读）。
    pub fn save(&self) {
        if let Some(p) = Self::path() {
            let _ = self.save_to(&p);
        }
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, self.to_toml())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let c = Config {
            theme: Some("gruvbox".into()),
            safe_zone: Some(2),
            line_gap: Some(1),
            para_gap: Some(2),
            fold_default: Some(true),
        };
        let parsed = Config::parse(&c.to_toml());
        assert_eq!(c, parsed);
    }

    #[test]
    fn parse_tolerates_noise() {
        let c = Config::parse("# 注释\ntheme = \"dark\"\n\nbad line\nsafe_zone = 0\nnonsense = 9\n");
        assert_eq!(c.theme.as_deref(), Some("dark"));
        assert_eq!(c.safe_zone, Some(0));
    }

    #[test]
    fn parse_defaults_on_garbage() {
        let c = Config::parse("theme = 12\nsafe_zone = \"x\"\n");
        assert_eq!(c, Config::default());
        assert_eq!(Config::parse("theme = \"solarized\""), Config::default());
    }

    #[test]
    fn save_to_creates_dirs() {
        let dir = std::env::temp_dir().join(format!("red-cfg-test-{}", std::process::id()));
        let path = dir.join("a/b/config.toml");
        let c = Config {
            theme: Some("dark".into()),
            safe_zone: Some(1),
            line_gap: Some(1),
            para_gap: Some(1),
            fold_default: Some(false),
        };
        c.save_to(&path).unwrap();
        assert_eq!(Config::parse(&fs::read_to_string(&path).unwrap()), c);
        fs::remove_dir_all(&dir).ok();
    }
}

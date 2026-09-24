use ratatui::style::Color;

/// NO_COLOR 环境变量（非空）表示禁用一切颜色。
pub fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// 终端支持的颜色数（保守探测：truecolor / 256 / 16）。
pub fn color_count() -> u32 {
    if no_color() {
        return 16;
    }
    if let Ok(ct) = std::env::var("COLORTERM") {
        let ct = ct.to_lowercase();
        if ct.contains("truecolor") || ct.contains("24bit") {
            return 16_777_216;
        }
    }
    match std::env::var("TERM") {
        Ok(t) if t.contains("256color") => 256,
        Ok(t) if !t.is_empty() && t != "dumb" => 16,
        _ => 16,
    }
}

/// 把颜色按终端能力降级；NO_COLOR 时返回终端默认色。
pub fn adapt(color: Color) -> Color {
    if no_color() {
        return Color::Reset;
    }
    match color {
        Color::Rgb(r, g, b) => match color_count() {
            n if n >= 16_777_216 => color,
            n if n >= 256 => Color::Indexed(rgb_to_256(r, g, b)),
            _ => rgb_to_16(r, g, b),
        },
        other => other,
    }
}

/// RGB → xterm 256 色索引（6×6×6 立方 + 灰阶带）。
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let (r16, g16, b16) = (u16::from(r), u16::from(g), u16::from(b));
    let max = r16.max(g16).max(b16);
    let min = r16.min(g16).min(b16);
    if max - min < 10 {
        let v = ((r16 + g16 + b16) / 3) as u8;
        if v > 238 {
            return 231; // 白
        }
        if v < 8 {
            return 16; // 黑
        }
        return 232 + ((u16::from(v) - 8) * 24 / 247) as u8;
    }
    let ir = (r16 * 5 / 255) as u8;
    let ig = (g16 * 5 / 255) as u8;
    let ib = (b16 * 5 / 255) as u8;
    16 + 36 * ir + 6 * ig + ib
}

/// RGB → 16 基础 ANSI 色（按 RGB 位与亮度选普通/高亮）。
pub fn rgb_to_16(r: u8, g: u8, b: u8) -> Color {
    let bit = |c: u8| u8::from(u32::from(c) > 127);
    let idx = bit(r) | (bit(g) << 1) | (bit(b) << 2);
    let lum = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
    Color::Indexed(if lum > 160 { idx + 8 } else { idx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_to_256_corners_and_gray() {
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        assert_eq!(rgb_to_256(255, 255, 255), 231);
        assert_eq!(rgb_to_256(255, 0, 0), 196);
        assert_eq!(rgb_to_256(0, 255, 0), 46);
        assert_eq!(rgb_to_256(0, 0, 255), 21);
        let g = rgb_to_256(128, 128, 128);
        assert!((232..=255).contains(&g), "灰色应落灰阶带: {g}");
    }

    #[test]
    fn rgb_to_16_basic() {
        assert_eq!(rgb_to_16(0, 0, 0), Color::Indexed(0));
        assert_eq!(rgb_to_16(255, 255, 0), Color::Indexed(11));
        assert_eq!(rgb_to_16(64, 64, 64), Color::Indexed(0));
        assert_eq!(rgb_to_16(0, 255, 255), Color::Indexed(14));
    }
}

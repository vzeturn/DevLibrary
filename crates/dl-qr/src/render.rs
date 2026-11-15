//! SVG and terminal text output.

use crate::QrCode;
use alloc::string::String;
use core::fmt::Write;

/// Options for [`QrCode::to_svg`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvgOptions<'a> {
    /// Quiet zone width in modules. The standard asks for 4.
    pub border: usize,
    /// Fill of dark modules (any SVG paint, e.g. `"#000"` or `"currentColor"`).
    pub dark: &'a str,
    /// Background fill. An empty string omits the background rectangle (transparent).
    pub light: &'a str,
    /// Pixels per module. `Some(n)` sets `width` / `height` attributes; `None` leaves the
    /// SVG unsized, so it scales to its container (coordinates are in modules).
    pub module_size: Option<u32>,
}

impl Default for SvgOptions<'_> {
    fn default() -> Self {
        Self {
            border: 4,
            dark: "#000",
            light: "#fff",
            module_size: None,
        }
    }
}

/// Writes `s` with XML attribute escaping.
fn push_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

impl QrCode {
    /// Whether the module at (x, y) of the symbol extended by `border` light modules is dark.
    fn dark_with_border(&self, x: usize, y: usize, border: usize) -> bool {
        match (x.checked_sub(border), y.checked_sub(border)) {
            (Some(x), Some(y)) => self.get(x, y),
            _ => false,
        }
    }

    /// Renders the symbol as a standalone SVG document.
    ///
    /// Dark modules are drawn as a single `<path>` made of merged horizontal runs, with
    /// `shape-rendering="crispEdges"` so that no seams appear between modules. Colours are
    /// XML-escaped.
    ///
    /// ```
    /// use dl_qr::{Ecc, QrCode, SvgOptions};
    ///
    /// let qr = QrCode::encode_text("hello", Ecc::Medium)?;
    /// let svg = qr.to_svg(&SvgOptions::default());
    /// assert!(svg.starts_with("<svg") && svg.contains("viewBox=\"0 0 29 29\""));
    /// # Ok::<(), dl_qr::Error>(())
    /// ```
    pub fn to_svg(&self, options: &SvgOptions<'_>) -> String {
        let n = self.size();
        let b = options.border;
        let full = n.saturating_add(b.saturating_mul(2));
        // `n <= 177`, so the capacity hint cannot overflow.
        let mut out = String::with_capacity(256 + n * n * 3);
        // `write!` into a String cannot fail.
        let _ = write!(
            out,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\" viewBox=\"0 0 {full} {full}\""
        );
        if let Some(px) = options.module_size {
            let side = (full as u64).saturating_mul(px as u64);
            let _ = write!(out, " width=\"{side}\" height=\"{side}\"");
        }
        out.push_str(" shape-rendering=\"crispEdges\">");
        if !options.light.is_empty() {
            out.push_str("<rect width=\"100%\" height=\"100%\" fill=\"");
            push_escaped(&mut out, options.light);
            out.push_str("\"/>");
        }
        out.push_str("<path fill=\"");
        push_escaped(&mut out, options.dark);
        out.push_str("\" d=\"");
        let mut first = true;
        for y in 0..n {
            let mut x = 0;
            while x < n {
                if !self.get(x, y) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < n && self.get(x, y) {
                    x += 1;
                }
                let len = x - start;
                if !first {
                    out.push(' ');
                }
                first = false;
                let (px, py) = (start.saturating_add(b), y.saturating_add(b));
                let _ = write!(out, "M{px},{py}h{len}v1h-{len}z");
            }
        }
        out.push_str("\"/></svg>\n");
        out
    }

    /// Renders the symbol for a terminal using Unicode half blocks: each character covers
    /// two module rows (`█` both dark, `▀` top dark, `▄` bottom dark, space both light).
    /// `border` light modules are added on every side; lines end with `\n`. The output
    /// grows with the square of `size() + 2 * border`, so keep the border small (4 is the
    /// standard quiet zone).
    ///
    /// Dark modules are printed as block characters, which assumes dark glyphs on a light
    /// background. On a dark-themed terminal the code appears inverted, which some
    /// scanners do not accept.
    ///
    /// ```
    /// use dl_qr::{Ecc, QrCode};
    ///
    /// let qr = QrCode::encode_text("hi", Ecc::Low)?;
    /// let text = qr.to_text(0);
    /// // 21 module rows in 11 lines of 21 characters; the finder's top edge is full blocks.
    /// assert_eq!(text.lines().count(), 11);
    /// assert!(text.starts_with("█▀▀▀▀▀█"));
    /// # Ok::<(), dl_qr::Error>(())
    /// ```
    pub fn to_text(&self, border: usize) -> String {
        let full = self.size().saturating_add(border.saturating_mul(2));
        // Up to 3 UTF-8 bytes per character plus a newline per line.
        let hint = full
            .checked_add(1)
            .and_then(|w| w.checked_mul(full.div_ceil(2)))
            .and_then(|c| c.checked_mul(3))
            .map_or(0, |c| c.min(1 << 20));
        let mut out = String::with_capacity(hint);
        let mut y = 0;
        while y < full {
            for x in 0..full {
                let top = self.dark_with_border(x, y, border);
                let bottom = y + 1 < full && self.dark_with_border(x, y + 1, border);
                out.push(match (top, bottom) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                });
            }
            out.push('\n');
            y += 2;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::{Ecc, QrCode, SvgOptions};
    use std::string::String;
    use std::vec::Vec;

    /// Rasterises the SVG path back into modules (it only uses `M x,y h len v1 h-len z`).
    fn parse_path(svg: &str, full: usize) -> Vec<Vec<bool>> {
        let d = svg
            .split(" d=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let mut grid = std::vec![std::vec![false; full]; full];
        for cmd in d.split(' ').filter(|c| !c.is_empty()) {
            let rest = cmd.strip_prefix('M').unwrap();
            let (xy, tail) = rest.split_once('h').unwrap();
            let (x, y) = xy.split_once(',').unwrap();
            let len: usize = tail.split_once('v').unwrap().0.parse().unwrap();
            assert_eq!(tail, std::format!("{len}v1h-{len}z"));
            let (x, y): (usize, usize) = (x.parse().unwrap(), y.parse().unwrap());
            for cell in &mut grid[y][x..x + len] {
                assert!(!*cell, "overlapping runs");
                *cell = true;
            }
        }
        grid
    }

    #[test]
    fn svg_round_trips() {
        let qr = QrCode::encode_text("https://example.com", Ecc::Medium).unwrap();
        let opts = SvgOptions {
            border: 2,
            dark: "#123",
            light: "#fff",
            module_size: Some(5),
        };
        let svg = qr.to_svg(&opts);
        let n = qr.size();
        let full = n + 4;
        assert!(svg.contains(&std::format!("viewBox=\"0 0 {full} {full}\"")));
        assert!(svg.contains(&std::format!("width=\"{}\"", full * 5)));
        assert!(svg.contains("shape-rendering=\"crispEdges\""));
        assert_eq!(svg.matches("<path").count(), 1);
        let grid = parse_path(&svg, full);
        for (y, row) in grid.iter().enumerate() {
            for (x, &dark) in row.iter().enumerate() {
                let expect = x >= 2 && y >= 2 && qr.get(x - 2, y - 2);
                assert_eq!(dark, expect, "({x},{y})");
            }
        }
    }

    #[test]
    fn svg_escapes_and_transparent() {
        let qr = QrCode::encode_text("x", Ecc::Low).unwrap();
        let svg = qr.to_svg(&SvgOptions {
            dark: "a\"<&",
            light: "",
            ..SvgOptions::default()
        });
        assert!(svg.contains("fill=\"a&quot;&lt;&amp;\""));
        assert!(!svg.contains("<rect"));
        assert!(!svg.contains("width="));
    }

    #[test]
    fn text_rendering() {
        let qr = QrCode::encode_text("text", Ecc::Low).unwrap();
        let n = qr.size();
        let s = qr.to_text(1);
        let lines: Vec<&str> = s.lines().collect();
        let full = n + 2;
        assert_eq!(lines.len(), full.div_ceil(2));
        assert!(lines.iter().all(|l| l.chars().count() == full));
        // Decode the half blocks back into rows.
        let mut rows: Vec<Vec<bool>> = Vec::new();
        for l in &lines {
            let (mut top, mut bottom) = (Vec::new(), Vec::new());
            for c in l.chars() {
                top.push(c == '█' || c == '▀');
                bottom.push(c == '█' || c == '▄');
            }
            rows.push(top);
            rows.push(bottom);
        }
        for (y, row) in rows.iter().enumerate().take(full) {
            for (x, &d) in row.iter().enumerate() {
                let expect = x >= 1 && y >= 1 && qr.get(x - 1, y - 1);
                assert_eq!(d, expect);
            }
        }
        // The extra half row below an odd height is light.
        assert!(rows[full].iter().all(|&d| !d));
        let none: String = qr.to_text(0);
        assert!(none.starts_with('█'));
    }
}

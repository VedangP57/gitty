//! sRGB colours: parsing, alpha blending, luminance, nearest xterm-256 index.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `#rrggbb` or `#rgb`.
    pub fn parse(s: &str) -> Option<Rgb> {
        let hex = s.strip_prefix('#')?;
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = |s: &str| u8::from_str_radix(s, 16).ok();
        match hex.len() {
            6 => Some(Rgb(v(&hex[0..2])?, v(&hex[2..4])?, v(&hex[4..6])?)),
            3 => {
                let d = |i: usize| v(&hex[i..i + 1]).map(|x| x * 17);
                Some(Rgb(d(0)?, d(1)?, d(2)?))
            }
            _ => None,
        }
    }

    /// `self` drawn at `alpha` opacity over `bg`.
    pub fn blend(self, bg: Rgb, alpha: f32) -> Rgb {
        let a = alpha.clamp(0.0, 1.0);
        let mix = |f: u8, b: u8| (f32::from(f) * a + f32::from(b) * (1.0 - a)).round() as u8;
        Rgb(mix(self.0, bg.0), mix(self.1, bg.1), mix(self.2, bg.2))
    }

    /// Relative luminance (Rec. 709 weights on linearised channels), 0..1.
    pub fn luma(self) -> f32 {
        let lin = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(self.0) + 0.7152 * lin(self.1) + 0.0722 * lin(self.2)
    }

    /// Nearest entry of the xterm 256-colour palette (6×6×6 cube or the 24-step gray ramp).
    pub fn to_xterm256(self) -> u8 {
        const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
        let near = |c: u8| {
            let c = i32::from(c);
            (0..6).min_by_key(|&i| (LEVELS[i] - c).abs()).unwrap_or(0)
        };
        let (r, g, b) = (near(self.0), near(self.1), near(self.2));
        let dist = |x: i32, y: i32, z: i32| {
            let d = |a: i32, c: u8| (a - i32::from(c)).pow(2);
            d(x, self.0) + d(y, self.1) + d(z, self.2)
        };
        let cube = dist(LEVELS[r], LEVELS[g], LEVELS[b]);
        let avg = (i32::from(self.0) + i32::from(self.1) + i32::from(self.2)) / 3;
        let gi = ((avg - 8 + 5) / 10).clamp(0, 23);
        let gv = 8 + 10 * gi;
        if dist(gv, gv, gv) < cube { (232 + gi) as u8 } else { (16 + 36 * r + 6 * g + b) as u8 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_forms() {
        assert_eq!(Rgb::parse("#fff"), Some(Rgb(255, 255, 255)));
        assert_eq!(Rgb::parse("#0d1117"), Some(Rgb(13, 17, 23)));
        assert_eq!(Rgb::parse("0d1117"), None);
        assert_eq!(Rgb::parse("bogus"), None);
        assert_eq!(Rgb::parse("#12345"), None);
        assert_eq!(Rgb::parse("#gggggg"), None);
    }

    #[test]
    fn blend_endpoints() {
        let (fg, bg) = (Rgb(200, 100, 0), Rgb(0, 0, 100));
        assert_eq!(fg.blend(bg, 0.0), bg);
        assert_eq!(fg.blend(bg, 1.0), fg);
        assert_eq!(fg.blend(bg, 0.5), Rgb(100, 50, 50));
    }

    #[test]
    fn luma_orders() {
        assert!(Rgb(255, 255, 255).luma() > 0.9);
        assert!(Rgb(13, 17, 23).luma() < 0.1);
    }

    #[test]
    fn xterm_mapping() {
        assert_eq!(Rgb(0, 0, 0).to_xterm256(), 16);
        assert_eq!(Rgb(255, 255, 255).to_xterm256(), 231);
        assert_eq!(Rgb(128, 128, 128).to_xterm256(), 244);
        assert_eq!(Rgb(255, 0, 0).to_xterm256(), 196);
        assert_eq!(Rgb(0x5f, 0x87, 0xaf).to_xterm256(), 67);
    }
}

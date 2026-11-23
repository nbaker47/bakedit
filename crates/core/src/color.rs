//! Colours.

/// A 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// Parse `#rgb`, `#rrggbb` or `#rrggbbaa` (the leading `#` is optional). The second value
    /// is the alpha, 255 when the string has none.
    pub fn parse_hex(text: &str) -> Option<(Rgb, u8)> {
        let hex = text.trim().trim_start_matches('#');
        if !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        match hex.len() {
            3 => {
                let nibble = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok().map(|n| n * 17);
                Some((Rgb::new(nibble(0)?, nibble(1)?, nibble(2)?), 255))
            }
            6 => Some((Rgb::new(byte(0)?, byte(2)?, byte(4)?), 255)),
            8 => Some((Rgb::new(byte(0)?, byte(2)?, byte(4)?), byte(6)?)),
            _ => None,
        }
    }

    /// This colour laid over `under` with the given alpha (0 = all `under`, 255 = all `self`).
    pub fn over(self, under: Rgb, alpha: u8) -> Rgb {
        let mix = |top: u8, bottom: u8| -> u8 {
            let a = alpha as u32;
            ((top as u32 * a + bottom as u32 * (255 - a) + 127) / 255) as u8
        };
        Rgb::new(mix(self.r, under.r), mix(self.g, under.g), mix(self.b, under.b))
    }

    /// Move `amount` (0.0..=1.0) of the way from this colour to `other`.
    pub fn lerp(self, other: Rgb, amount: f32) -> Rgb {
        let alpha = (amount.clamp(0.0, 1.0) * 255.0).round() as u8;
        other.over(self, alpha)
    }

    /// Perceived brightness, 0.0 (black) to 1.0 (white).
    pub fn luminance(self) -> f32 {
        (0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32) / 255.0
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// A terminal cell colour: the default, one of the 256 palette entries, or direct RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(Rgb),
}

impl From<Rgb> for Color {
    fn from(rgb: Rgb) -> Color {
        Color::Rgb(rgb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_forms() {
        assert_eq!(Rgb::parse_hex("#82aaff"), Some((Rgb::new(0x82, 0xaa, 0xff), 255)));
        assert_eq!(Rgb::parse_hex("82aaff55"), Some((Rgb::new(0x82, 0xaa, 0xff), 0x55)));
        assert_eq!(Rgb::parse_hex("#fff"), Some((Rgb::new(255, 255, 255), 255)));
        assert_eq!(Rgb::parse_hex("#12"), None);
        assert_eq!(Rgb::parse_hex("#gggggg"), None);
        assert_eq!(Rgb::parse_hex("#ééé"), None);
    }

    #[test]
    fn blends_over_a_background() {
        let white = Rgb::new(255, 255, 255);
        let black = Rgb::new(0, 0, 0);
        assert_eq!(white.over(black, 255), white);
        assert_eq!(white.over(black, 0), black);
        assert_eq!(white.over(black, 128), Rgb::new(128, 128, 128));
        assert_eq!(black.lerp(white, 1.0), white);
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(Rgb::new(0x12, 0x10, 0x1a).to_hex(), "#12101a");
    }
}

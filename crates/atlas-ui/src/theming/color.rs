//! The [`Color`] value type with hex-string and RGB-array (de)serialization.
//!
//! Colors are stored internally as a packed `u32` in `0xRRGGBBAA` order.

use std::fmt;

use serde::{Deserializer, Serializer};

use super::watcher::ThemeError;

/// A 32-bit color value stored as `0xRRGGBBAA`.
///
/// Accepts hex strings (`#RRGGBB`, `#RRGGBBAA`) and integer arrays
/// (`[r, g, b]` or `[r, g, b, a]`) in TOML/JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    /// Parse a CSS-style hex color string.
    ///
    /// Accepts `#RRGGBB` (alpha defaults to `0xFF`) and `#RRGGBBAA`.
    /// The leading `#` is required; the string is case-insensitive.
    ///
    /// # Errors
    ///
    /// Returns [`ThemeError::InvalidColor`] when the format is wrong.
    pub fn from_hex_str(s: &str) -> Result<Self, ThemeError> {
        let hex = s.trim().strip_prefix('#').ok_or_else(|| {
            ThemeError::InvalidColor(format!("expected '#' prefix in color: {s:?}"))
        })?;
        match hex.len() {
            6 => {
                let v = u32::from_str_radix(hex, 16)
                    .map_err(|_| ThemeError::InvalidColor(format!("invalid hex color: {s:?}")))?;
                Ok(Self((v << 8) | 0xFF))
            }
            8 => {
                let v = u32::from_str_radix(hex, 16)
                    .map_err(|_| ThemeError::InvalidColor(format!("invalid hex color: {s:?}")))?;
                Ok(Self(v))
            }
            _ => Err(ThemeError::InvalidColor(format!(
                "hex color must be #RRGGBB or #RRGGBBAA, got {s:?}"
            ))),
        }
    }

    /// Serialize to a CSS hex string.
    ///
    /// Produces `#RRGGBB` when alpha is `0xFF`, otherwise `#RRGGBBAA`.
    pub fn to_hex_str(self) -> String {
        let (r, g, b, a) = self.rgba_components();
        if a == 0xFF {
            format!("#{r:02X}{g:02X}{b:02X}")
        } else {
            format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
        }
    }

    /// Return the individual `(r, g, b, a)` components, each in `0..=255`.
    pub fn rgba_components(self) -> (u8, u8, u8, u8) {
        let v = self.0;
        (
            ((v >> 24) & 0xFF) as u8,
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        )
    }

    /// Convert to a [`slint::Color`] (ARGB layout).
    pub fn to_slint_color(self) -> slint::Color {
        let (r, g, b, a) = self.rgba_components();
        slint::Color::from_argb_u8(a, r, g, b)
    }
}

impl serde::Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex_str())
    }
}

impl<'de> serde::Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ColorVisitor)
    }
}

struct ColorVisitor;

impl<'de> serde::de::Visitor<'de> for ColorVisitor {
    type Value = Color;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a hex color string (#RRGGBB or #RRGGBBAA) or an integer array [r, g, b] or [r, g, b, a]"
        )
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Color, E> {
        Color::from_hex_str(value).map_err(|error| E::custom(error.to_string()))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Color, A::Error> {
        let r: u8 = seq.next_element()?.ok_or_else(|| {
            serde::de::Error::custom("color array must have at least 3 elements (r, g, b)")
        })?;
        let g: u8 = seq.next_element()?.ok_or_else(|| {
            serde::de::Error::custom("color array must have at least 3 elements (r, g, b)")
        })?;
        let b: u8 = seq.next_element()?.ok_or_else(|| {
            serde::de::Error::custom("color array must have at least 3 elements (r, g, b)")
        })?;
        let a: u8 = seq.next_element()?.unwrap_or(0xFF);
        if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "color array must have 3 or 4 elements, got more",
            ));
        }
        Ok(Color(
            ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | (a as u32),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Wrapper {
        c: Color,
    }

    #[test]
    fn parse_rrggbb() {
        let color = Color::from_hex_str("#0e1116").expect("valid color");
        assert_eq!(color.rgba_components(), (0x0e, 0x11, 0x16, 0xFF));
    }

    #[test]
    fn parse_rrggbbaa() {
        let color = Color::from_hex_str("#0e111680").expect("valid color");
        assert_eq!(color.rgba_components(), (0x0e, 0x11, 0x16, 0x80));
    }

    #[test]
    fn parse_lowercase() {
        let color = Color::from_hex_str("#aabbcc").expect("valid color");
        assert_eq!(color.rgba_components(), (0xAA, 0xBB, 0xCC, 0xFF));
    }

    #[test]
    fn parse_array_rgb() {
        let wrapper: Wrapper = toml::from_str("c = [14, 17, 22]").expect("valid toml");
        assert_eq!(wrapper.c.rgba_components(), (14, 17, 22, 0xFF));
    }

    #[test]
    fn parse_array_rgba() {
        let wrapper: Wrapper = toml::from_str("c = [14, 17, 22, 128]").expect("valid toml");
        assert_eq!(wrapper.c.rgba_components(), (14, 17, 22, 128));
    }

    #[test]
    fn rejects_invalid_hex() {
        assert!(Color::from_hex_str("0e1116").is_err());
        assert!(Color::from_hex_str("#xyz").is_err());
        assert!(Color::from_hex_str("#12345").is_err());
    }

    #[test]
    fn round_trip_hex() {
        let original = Color::from_hex_str("#2f81f7").expect("valid color");
        let reparsed = Color::from_hex_str(&original.to_hex_str()).expect("valid color");
        assert_eq!(original, reparsed);
    }

    #[test]
    fn round_trip_hex_with_alpha() {
        let original = Color::from_hex_str("#2f81f780").expect("valid color");
        let reparsed = Color::from_hex_str(&original.to_hex_str()).expect("valid color");
        assert_eq!(original, reparsed);
    }

    #[test]
    fn serialize_opaque_omits_alpha() {
        let color = Color::from_hex_str("#aabbcc").expect("valid color");
        assert_eq!(color.to_hex_str(), "#AABBCC");
    }

    #[test]
    fn serialize_transparent_includes_alpha() {
        let color = Color::from_hex_str("#aabbcc80").expect("valid color");
        assert_eq!(color.to_hex_str(), "#AABBCC80");
    }
}

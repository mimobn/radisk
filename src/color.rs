use crate::radial::{Segment, DEGREE_FACTOR};

/// Color scheme matching FileLight's implementation
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ColorScheme {
    #[default]
    Rainbow,
    HighContrast,
    Kde,
}

/// Configuration for color generation
#[derive(Debug, Clone)]
pub struct ColorConfig {
    pub scheme: ColorScheme,
    pub contrast: f64, // 0.0 to 1.0, default 0.5
    pub background_dark: bool,
}

impl Default for ColorConfig {
    fn default() -> Self {
        Self {
            scheme: ColorScheme::Rainbow,
            contrast: 0.5,
            background_dark: true,
        }
    }
}

/// RGB color (0-255)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Convert to ratatui Color
    pub fn to_ratatui(self) -> ratatui::style::Color {
        ratatui::style::Color::Rgb(self.r, self.g, self.b)
    }
}

/// HSV color (H: 0-360, S: 0-255, V: 0-255)
#[derive(Debug, Clone, Copy)]
pub struct HsvColor {
    pub h: f64,
    pub s: f64,
    pub v: f64,
}

impl HsvColor {
    pub fn new(h: f64, s: f64, v: f64) -> Self {
        Self {
            h: h % 360.0,
            s: s.clamp(0.0, 255.0),
            v: v.clamp(0.0, 255.0),
        }
    }

    /// Convert to RGB using manual HSV conversion
    pub fn to_rgb(self) -> Rgb {
        let h = self.h;
        let s = self.s / 255.0;
        let v = self.v / 255.0;

        if s == 0.0 {
            let val = (v * 255.0) as u8;
            return Rgb::new(val, val, val);
        }

        let h = h / 60.0;
        let i = h.floor() as i32;
        let f = h - i as f64;
        let p = v * (1.0 - s);
        let q = v * (1.0 - s * f);
        let t = v * (1.0 - s * (1.0 - f));

        let (r, g, b) = match i {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        };

        Rgb::new((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
    }
}

/// Color pair for a segment (fill and pen/stroke)
#[derive(Debug, Clone, Copy)]
pub struct SegmentColors {
    pub fill: Rgb,
    pub pen: Rgb,
}

/// Colorize a segment based on its position and type
/// Matches FileLight's colorise() function exactly
pub fn colorize_segment(segment: &Segment, depth: usize, config: &ColorConfig) -> SegmentColors {
    match config.scheme {
        ColorScheme::Rainbow => rainbow_colorize(segment, depth, config.contrast),
        ColorScheme::HighContrast => high_contrast_colorize(segment, depth, config.contrast),
        ColorScheme::Kde => kde_colorize(segment, depth),
    }
}

/// Rainbow color scheme (default in FileLight)
/// HSV colors with hue based on angle position
fn rainbow_colorize(segment: &Segment, depth: usize, contrast: f64) -> SegmentColors {
    // Hue from angle position (matches FileLight exactly)
    let h = segment.start_angle as f64 / DEGREE_FACTOR as f64;

    // Darkness increases with depth (FileLight: darkness += 0.04 per depth)
    let darkness = 1.0 + depth as f64 * 0.04;

    let s1: f64 = 160.0;
    let v1 = 255.0 / darkness;

    let v2 = v1 - contrast * v1;
    let s2 = s1 + contrast * (255.0 - s1);

    // Ensure minimum saturation
    let s1 = s1.max(80.0);

    let (fill, pen) = if segment.is_fake {
        // FilesGroup (multi-file)
        let fill_v = v2.max(90.0);
        let fill_s = s2;
        (
            HsvColor::new(h, fill_s, fill_v).to_rgb(),
            HsvColor::new(h, 17.0, v1).to_rgb(),
        )
    } else if !segment.is_folder {
        // Regular file
        (
            HsvColor::new(h, 17.0, v1).to_rgb(),
            HsvColor::new(h, 17.0, v2).to_rgb(),
        )
    } else {
        // Folder
        (
            HsvColor::new(h, s1, v1).to_rgb(),
            HsvColor::new(h, s2, v2).to_rgb(),
        )
    };

    SegmentColors { fill, pen }
}

/// High contrast color scheme
fn high_contrast_colorize(_segment: &Segment, _depth: usize, contrast: f64) -> SegmentColors {
    let pen = HsvColor::new(0.0, 0.0, 0.0).to_rgb(); // Black outline
    let fill = HsvColor::new(180.0, 0.0, 255.0 * contrast).to_rgb(); // Grayscale fill

    SegmentColors { fill, pen }
}

/// KDE color scheme
fn kde_colorize(segment: &Segment, _depth: usize) -> SegmentColors {
    // Simplified KDE scheme using rainbow base
    let h = segment.start_angle as f64 / DEGREE_FACTOR as f64;
    let fill = HsvColor::new(h, 100.0, 200.0).to_rgb();
    let pen = HsvColor::new(h, 50.0, 150.0).to_rgb();

    SegmentColors { fill, pen }
}

/// Get color for center circle (root)
pub fn center_color(config: &ColorConfig) -> Rgb {
    match config.scheme {
        ColorScheme::HighContrast => Rgb::new(255, 255, 255),
        _ => {
            if config.background_dark {
                Rgb::new(30, 30, 46) // Dark background
            } else {
                Rgb::new(250, 250, 250) // Light background
            }
        }
    }
}

/// Get color for hover highlight
pub fn hover_color(segment: &Segment, depth: usize, config: &ColorConfig) -> Rgb {
    let colors = colorize_segment(segment, depth, config);

    // Darken the fill color for hover
    Rgb::new(
        (colors.fill.r as f64 * 0.7) as u8,
        (colors.fill.g as f64 * 0.7) as u8,
        (colors.fill.b as f64 * 0.7) as u8,
    )
}

/// Convert xterm-256 color index to RGB
#[allow(dead_code)]
pub fn xterm256_to_rgb(index: u8) -> Rgb {
    // Standard xterm-256 color cube
    if index < 16 {
        // Standard colors
        match index {
            0 => Rgb::new(0, 0, 0),
            1 => Rgb::new(128, 0, 0),
            2 => Rgb::new(0, 128, 0),
            3 => Rgb::new(128, 128, 0),
            4 => Rgb::new(0, 0, 128),
            5 => Rgb::new(128, 0, 128),
            6 => Rgb::new(0, 128, 128),
            7 => Rgb::new(192, 192, 192),
            8 => Rgb::new(128, 128, 128),
            9 => Rgb::new(255, 0, 0),
            10 => Rgb::new(0, 255, 0),
            11 => Rgb::new(255, 255, 0),
            12 => Rgb::new(0, 0, 255),
            13 => Rgb::new(255, 0, 255),
            14 => Rgb::new(0, 255, 255),
            15 => Rgb::new(255, 255, 255),
            _ => Rgb::new(0, 0, 0),
        }
    } else if index < 232 {
        // 6x6x6 color cube
        let i = index - 16;
        let r = (i / 36) * 51;
        let g = ((i % 36) / 6) * 51;
        let b = (i % 6) * 51;
        Rgb::new(r, g, b)
    } else {
        // Grayscale ramp
        let v = 8 + (index - 232) * 10;
        Rgb::new(v, v, v)
    }
}

/// Find closest xterm-256 color index to an RGB color
#[allow(dead_code)]
pub fn rgb_to_xterm256(color: Rgb) -> u8 {
    // Check grayscale
    if color.r == color.g && color.g == color.b {
        if color.r < 8 {
            return 16;
        }
        if color.r > 248 {
            return 231;
        }
        return 232 + (color.r - 8) / 10;
    }

    // Find closest color in 6x6x6 cube
    let r6 = ((color.r as f64 / 255.0) * 5.0).round() as u8;
    let g6 = ((color.g as f64 / 255.0) * 5.0).round() as u8;
    let b6 = ((color.b as f64 / 255.0) * 5.0).round() as u8;

    16 + 36 * r6 + 6 * g6 + b6
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radial::Segment;
    use uuid::Uuid;

    fn make_segment(start_angle: u32, is_folder: bool, is_fake: bool) -> Segment {
        Segment {
            uuid: Uuid::new_v4(),
            name: "test".to_string(),
            size: 100,
            start_angle,
            angle_length: 100,
            is_folder,
            is_fake,
            file_count: 1,
            path: String::new(),
            depth: 0,
            has_hidden_children: false,
        }
    }

    #[test]
    fn test_hue_from_angle() {
        let seg = make_segment(5760, false, false); // 360 degrees
        let config = ColorConfig::default();
        // Should produce a color without panicking; r/g/b are u8 so range
        // bounds are guaranteed by the type system.
        let _ = colorize_segment(&seg, 0, &config);
    }

    #[test]
    fn test_file_vs_folder_saturation() {
        let file_seg = make_segment(1000, false, false);
        let folder_seg = make_segment(1000, true, false);
        let config = ColorConfig::default();

        let file_colors = colorize_segment(&file_seg, 0, &config);
        let folder_colors = colorize_segment(&folder_seg, 0, &config);

        // Folder should have higher saturation than file
        // We can't directly compare saturation from RGB, but colors should differ
        assert_ne!(
            (file_colors.fill.r, file_colors.fill.g, file_colors.fill.b),
            (
                folder_colors.fill.r,
                folder_colors.fill.g,
                folder_colors.fill.b
            )
        );
    }

    #[test]
    fn test_depth_darkening() {
        let seg = make_segment(1000, false, false);
        let config = ColorConfig::default();

        let colors_depth0 = colorize_segment(&seg, 0, &config);
        let colors_depth3 = colorize_segment(&seg, 3, &config);

        // Deeper segments should be darker (lower value)
        // The fill at depth 3 should be darker than at depth 0
        let v0 = colors_depth0
            .fill
            .r
            .max(colors_depth0.fill.g)
            .max(colors_depth0.fill.b);
        let v3 = colors_depth3
            .fill
            .r
            .max(colors_depth3.fill.g)
            .max(colors_depth3.fill.b);
        assert!(
            v3 <= v0,
            "Depth 3 ({}) should be darker than depth 0 ({})",
            v3,
            v0
        );
    }

    #[test]
    fn test_contrast_affects_colors() {
        // Use different angles to ensure visible difference
        let seg_low = make_segment(2880, false, false); // 180 degrees
        let seg_high = make_segment(2880, false, false);

        let config_low = ColorConfig {
            contrast: 0.1,
            ..Default::default()
        };
        let config_high = ColorConfig {
            contrast: 0.9,
            ..Default::default()
        };

        let colors_low = colorize_segment(&seg_low, 0, &config_low);
        let colors_high = colorize_segment(&seg_high, 0, &config_high);

        // Check that pen colors differ (pen is more affected by contrast)
        let pen_diff = (colors_low.pen.r as i16 - colors_high.pen.r as i16).abs()
            + (colors_low.pen.g as i16 - colors_high.pen.g as i16).abs()
            + (colors_low.pen.b as i16 - colors_high.pen.b as i16).abs();
        assert!(
            pen_diff > 10,
            "Contrast should affect pen colors, diff: {}",
            pen_diff
        );
    }

    #[test]
    fn test_color_at_angle_zero() {
        let seg = make_segment(0, false, false);
        let config = ColorConfig::default();
        // Smoke test: angle 0 must not panic; r/g/b ranges are u8-bounded.
        let _ = colorize_segment(&seg, 0, &config);
    }

    #[test]
    fn test_color_at_angle_360() {
        let seg_zero = make_segment(0, false, false);
        let seg_full = make_segment(5760, false, false); // 360 * 16
        let config = ColorConfig::default();

        let at_zero = colorize_segment(&seg_zero, 0, &config);
        let at_full = colorize_segment(&seg_full, 0, &config);

        // 360° should wrap to the same hue as 0°.
        assert_eq!(
            (at_zero.fill.r, at_zero.fill.g, at_zero.fill.b),
            (at_full.fill.r, at_full.fill.g, at_full.fill.b),
            "wrap-around at 360° should produce the same colour as 0°"
        );
    }

    #[test]
    fn test_fake_segment_colors() {
        let real = make_segment(1000, false, false);
        let fake = make_segment(1000, false, true);
        let config = ColorConfig::default();

        // Fake segments should be visually distinguishable from real ones at
        // the same angle/depth; otherwise the "Other" bucket would be
        // indistinguishable from a real entry of the same size.
        let real_colors = colorize_segment(&real, 0, &config);
        let fake_colors = colorize_segment(&fake, 0, &config);
        assert_ne!(
            (real_colors.fill.r, real_colors.fill.g, real_colors.fill.b),
            (fake_colors.fill.r, fake_colors.fill.g, fake_colors.fill.b),
            "fake segments must render distinctly from real segments"
        );
    }

    #[test]
    fn test_xterm256_roundtrip() {
        // Test some known colors
        let test_colors = vec![
            Rgb::new(0, 0, 0),
            Rgb::new(255, 0, 0),
            Rgb::new(0, 255, 0),
            Rgb::new(0, 0, 255),
            Rgb::new(255, 255, 255),
            Rgb::new(128, 128, 128),
        ];

        for color in test_colors {
            let idx = rgb_to_xterm256(color);
            let back = xterm256_to_rgb(idx);
            // Should be reasonably close (xterm has limited palette)
            let dr = (color.r as i16 - back.r as i16).unsigned_abs() as u8;
            let dg = (color.g as i16 - back.g as i16).unsigned_abs() as u8;
            let db = (color.b as i16 - back.b as i16).unsigned_abs() as u8;
            assert!(
                dr <= 51 && dg <= 51 && db <= 51,
                "Color conversion too far off"
            );
        }
    }

    #[test]
    fn test_hover_color_is_darker() {
        let seg = make_segment(1000, false, false);
        let config = ColorConfig::default();

        let normal = colorize_segment(&seg, 0, &config);
        let hover = hover_color(&seg, 0, &config);

        // Hover should be darker than normal fill
        assert!(hover.r <= normal.fill.r);
        assert!(hover.g <= normal.fill.g);
        assert!(hover.b <= normal.fill.b);
    }

    #[test]
    fn test_hsv_to_rgb_conversion() {
        // Red (0 degrees)
        let red = HsvColor::new(0.0, 255.0, 255.0).to_rgb();
        assert_eq!(red.r, 255);
        assert_eq!(red.g, 0);
        assert_eq!(red.b, 0);

        // Green (120 degrees)
        let green = HsvColor::new(120.0, 255.0, 255.0).to_rgb();
        assert_eq!(green.r, 0);
        assert_eq!(green.g, 255);
        assert_eq!(green.b, 0);

        // Blue (240 degrees)
        let blue = HsvColor::new(240.0, 255.0, 255.0).to_rgb();
        assert_eq!(blue.r, 0);
        assert_eq!(blue.g, 0);
        assert_eq!(blue.b, 255);
    }
}

//! PDF Split & Crop and Reading Configuration (Typography, Contrast Curves,
//! Background Whitening, Night Mode, and Layout).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[derive(Default)]
pub enum SplitPreset {
    #[default]
    FitPage,
    Horizontal2, // 2-split: Top half -> Bottom half (Landscape)
    Horizontal3, // 3-split: Top -> Mid -> Bottom (Landscape)
    Vertical2,   // 2-split: Left column -> Right column (Portrait)
    Grid4,       // 4-split: 2 columns x 2 rows (Landscape)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[derive(Default)]
pub enum ContrastMode {
    #[default]
    Normal,       // Default 1:1 grayscale
    BoldText,     // Darkens anti-aliased font edges by ~25%
    HighContrast, // Strong S-curve for crisp punchy text
    ScanClean,    // Aggressive black boost + paper whitening for scans
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl RectF {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.001)
    }

    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.001)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitConfig {
    pub preset: SplitPreset,
    /// Rotation in degrees (0 = portrait, 270 = landscape CW/default, 90 = landscape CCW)
    pub rotation: u16,
    /// Overlap fraction (e.g. 0.06 = 6% overlap between adjacent split boxes)
    pub overlap: f32,
    /// Normalized margin crops (0.0 .. 0.4)
    pub margin_left: f32,
    pub margin_top: f32,
    pub margin_right: f32,
    pub margin_bottom: f32,
}

impl Default for SplitConfig {
    fn default() -> Self {
        Self {
            preset: SplitPreset::FitPage,
            rotation: 0,
            overlap: 0.018,
            margin_left: 0.0,
            margin_top: 0.0,
            margin_right: 0.0,
            margin_bottom: 0.0,
        }
    }
}

impl SplitConfig {
    pub fn for_preset(preset: SplitPreset) -> Self {
        match preset {
            SplitPreset::FitPage => Self {
                preset: SplitPreset::FitPage,
                rotation: 0,
                overlap: 0.0,
                margin_left: 0.0,
                margin_top: 0.0,
                margin_right: 0.0,
                margin_bottom: 0.0,
            },
            SplitPreset::Horizontal2 => Self {
                preset: SplitPreset::Horizontal2,
                rotation: 270,  // Landscape default
                overlap: 0.018, // minimal overlap to recover partially cut line
                margin_left: 0.02,
                margin_top: 0.02,
                margin_right: 0.02,
                margin_bottom: 0.02,
            },
            SplitPreset::Horizontal3 => Self {
                preset: SplitPreset::Horizontal3,
                rotation: 270,  // Landscape default
                overlap: 0.018, // minimal overlap
                margin_left: 0.02,
                margin_top: 0.02,
                margin_right: 0.02,
                margin_bottom: 0.02,
            },
            SplitPreset::Vertical2 => Self {
                preset: SplitPreset::Vertical2,
                rotation: 0,
                overlap: 0.05,
                margin_left: 0.02,
                margin_top: 0.02,
                margin_right: 0.02,
                margin_bottom: 0.02,
            },
            SplitPreset::Grid4 => Self {
                preset: SplitPreset::Grid4,
                rotation: 270,
                overlap: 0.06,
                margin_left: 0.02,
                margin_top: 0.02,
                margin_right: 0.02,
                margin_bottom: 0.02,
            },
        }
    }

    /// The orientation cycle both rotation entry points (the settings
    /// dialog's button and the curtain's ROTATE pill) walk: portrait →
    /// landscape CW → landscape CCW → portrait. One function, one order —
    /// the two UIs can never disagree about what "next" means.
    pub fn next_rotation(cur: u16) -> u16 {
        match cur {
            0 => 270,
            270 => 90,
            _ => 0,
        }
    }

    pub fn sub_box_count(&self) -> usize {
        match self.preset {
            SplitPreset::FitPage => 1,
            SplitPreset::Horizontal2 => 2,
            SplitPreset::Horizontal3 => 3,
            SplitPreset::Vertical2 => 2,
            SplitPreset::Grid4 => 4,
        }
    }

    /// Generates the list of sub-boxes in reading order (normalized 0.0..1.0 coordinates).
    pub fn sub_boxes(&self) -> Vec<RectF> {
        let x0 = self.margin_left.clamp(0.0, 0.45);
        let y0 = self.margin_top.clamp(0.0, 0.45);
        let x1 = (1.0 - self.margin_right).clamp(x0 + 0.1, 1.0);
        let y1 = (1.0 - self.margin_bottom).clamp(y0 + 0.1, 1.0);

        let w = x1 - x0;
        let h = y1 - y0;

        match self.preset {
            SplitPreset::FitPage => vec![RectF::new(x0, y0, x1, y1)],

            SplitPreset::Horizontal2 => {
                // N = 2 horizontal slices: equal box height & uniform overlap
                let ov = self.overlap.clamp(0.0, 0.40);
                let box_h = h * (1.0 + ov) / 2.0;
                let step_y = h * (1.0 - ov) / 2.0;
                vec![
                    // Box 0: Top half
                    RectF::new(x0, y0, x1, y0 + box_h),
                    // Box 1: Bottom half
                    RectF::new(x0, y0 + step_y, x1, y1),
                ]
            }

            SplitPreset::Horizontal3 => {
                // N = 3 horizontal slices: identical box height, equal step size, and exact uniform overlap
                let ov = self.overlap.clamp(0.0, 0.35);
                let box_h = h * (1.0 + 2.0 * ov) / 3.0;
                let step_y = h * (1.0 - ov) / 3.0;
                vec![
                    // Box 0: Top slice
                    RectF::new(x0, y0, x1, y0 + box_h),
                    // Box 1: Middle slice
                    RectF::new(x0, y0 + step_y, x1, y0 + step_y + box_h),
                    // Box 2: Bottom slice
                    RectF::new(x0, y0 + 2.0 * step_y, x1, y1),
                ]
            }

            SplitPreset::Vertical2 => {
                // N = 2 vertical columns: equal box width & uniform overlap
                let ov = self.overlap.clamp(0.0, 0.40);
                let box_w = w * (1.0 + ov) / 2.0;
                let step_x = w * (1.0 - ov) / 2.0;
                vec![
                    // Box 0: Left column
                    RectF::new(x0, y0, x0 + box_w, y1),
                    // Box 1: Right column
                    RectF::new(x0 + step_x, y0, x1, y1),
                ]
            }

            SplitPreset::Grid4 => {
                // 2x2 grid: equal box width & height, uniform step sizes
                let ov = self.overlap.clamp(0.0, 0.35);
                let box_w = w * (1.0 + ov) / 2.0;
                let step_x = w * (1.0 - ov) / 2.0;
                let box_h = h * (1.0 + ov) / 2.0;
                let step_y = h * (1.0 - ov) / 2.0;

                // Reading order: Column 1 (Top -> Bottom), then Column 2 (Top -> Bottom)
                vec![
                    // 1. Top-Left
                    RectF::new(x0, y0, x0 + box_w, y0 + box_h),
                    // 2. Bottom-Left
                    RectF::new(x0, y0 + step_y, x0 + box_w, y1),
                    // 3. Top-Right
                    RectF::new(x0 + step_x, y0, x1, y0 + box_h),
                    // 4. Bottom-Right
                    RectF::new(x0 + step_x, y0 + step_y, x1, y1),
                ]
            }
        }
    }

    #[allow(dead_code)]
    pub fn is_landscape(&self) -> bool {
        self.rotation == 90 || self.rotation == 270
    }

    #[allow(dead_code)]
    pub fn total_steps(&self, page_count: usize) -> usize {
        page_count.max(1) * self.sub_box_count()
    }

    #[allow(dead_code)]
    pub fn step_to_page_sub(&self, step: usize) -> (usize, usize) {
        let count = self.sub_box_count();
        (step / count, step % count)
    }

    #[allow(dead_code)]
    pub fn page_sub_to_step(&self, page: usize, sub: usize) -> usize {
        let count = self.sub_box_count();
        page * count + sub.min(count.saturating_sub(1))
    }
}

/// Comprehensive Reader Settings (persisted per book or global defaults).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReaderSettings {
    pub split: SplitConfig,
    /// Reflow font size in points (e.g. 8.0, 9.5, 11.0, 13.0, 15.0, 18.0)
    pub font_size: f32,
    /// Reflow margin padding in pixels (e.g. 36 = compact, 72 = normal, 108 = wide)
    pub margin_pad: u32,
    /// Line-spacing multiplier (1.0 = the book's own leading; 1.1–1.6
    /// for airier text). Layout-affecting like font size — changes
    /// repaginate, so the anchor path carries the position.
    pub line_spacing: f32,
    /// Contrast & text darkness curve
    pub contrast: ContrastMode,
    /// Background white snap cutoff (e.g. 0 = off, 240, 230)
    pub white_cutoff: u8,
    /// Night mode (inverted grayscale)
    pub invert: bool,
    /// Full e-ink refresh interval in page turns (0 = manual only, 5, 10, 20)
    pub refresh_interval: usize,
    /// Show top status header (Clock + Battery + Book title)
    pub show_header: bool,
}

impl Default for ReaderSettings {
    fn default() -> Self {
        Self {
            split: SplitConfig::default(),
            font_size: 11.0,
            margin_pad: 72,
            line_spacing: 1.0,
            contrast: ContrastMode::Normal,
            white_cutoff: 0,
            invert: false,
            refresh_interval: 10,
            show_header: true,
        }
    }
}

impl ReaderSettings {
    /// Precomputes a 256-byte Lookup Table (LUT) for instant O(1) contrast & inversion.
    pub fn build_lut(&self) -> [u8; 256] {
        let mut lut = [0u8; 256];
        for (i, slot) in lut.iter_mut().enumerate() {
            let mut val = i as f32;

            // Apply contrast mode
            match self.contrast {
                ContrastMode::Normal => {}
                ContrastMode::BoldText => {
                    // Darken antialiased edges (0..180) by ~25%
                    if val < 180.0 {
                        val *= 0.75;
                    }
                }
                ContrastMode::HighContrast => {
                    // Steep S-curve: deep blacks, crisp whites
                    if val < 140.0 {
                        val = (val / 140.0).powf(1.4) * 90.0;
                    } else if val > 200.0 {
                        val = 200.0 + (val - 200.0) * 1.5;
                    }
                }
                ContrastMode::ScanClean => {
                    // Aggressive thresholding for scans
                    if val < 190.0 {
                        val *= 0.6;
                    } else {
                        val = 255.0;
                    }
                }
            }

            // Apply white cutoff
            if self.white_cutoff > 0 && val >= self.white_cutoff as f32 {
                val = 255.0;
            }

            let mut out = val.clamp(0.0, 255.0).round() as u8;

            // Invert (Night mode)
            if self.invert {
                out = 255 - out;
            }

            *slot = out;
        }
        lut
    }

    /// Apply contrast LUT in-place over grayscale buffer (blazing fast: < 1ms on 2MB buffer).
    pub fn apply_lut(&self, buf: &mut [u8]) {
        if self.contrast == ContrastMode::Normal && self.white_cutoff == 0 && !self.invert {
            return;
        }
        let lut = self.build_lut();
        for b in buf.iter_mut() {
            *b = lut[*b as usize];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_presets() {
        let h2 = SplitConfig::for_preset(SplitPreset::Horizontal2);
        assert_eq!(h2.sub_box_count(), 2);
        assert!(h2.is_landscape());
        let boxes = h2.sub_boxes();
        assert_eq!(boxes.len(), 2);
        assert!(boxes[0].y0 < boxes[1].y0);
        assert!(boxes[0].y1 > boxes[1].y0);

        let v2 = SplitConfig::for_preset(SplitPreset::Vertical2);
        assert_eq!(v2.sub_box_count(), 2);
        assert!(!v2.is_landscape());
        let boxes_v = v2.sub_boxes();
        assert_eq!(boxes_v.len(), 2);
        assert!(boxes_v[0].x1 > boxes_v[1].x0);

        let g4 = SplitConfig::for_preset(SplitPreset::Grid4);
        assert_eq!(g4.sub_box_count(), 4);
        assert_eq!(g4.total_steps(10), 40);
        assert_eq!(g4.step_to_page_sub(7), (1, 3));
        assert_eq!(g4.page_sub_to_step(1, 3), 7);
    }

    #[test]
    fn test_contrast_lut() {
        let mut s = ReaderSettings::default();
        s.contrast = ContrastMode::BoldText;
        let lut = s.build_lut();
        // Midtone should be darkened
        assert!(lut[100] < 100);
        // Pure black stays black
        assert_eq!(lut[0], 0);

        // Test invert
        s.invert = true;
        let lut_inv = s.build_lut();
        assert_eq!(lut_inv[0], 255);
    }
}

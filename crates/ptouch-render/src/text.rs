// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Text rendering using cosmic-text.
//!
//! Renders multi-line text into a [`LabelBitmap`] using system fonts.
//! Supports auto-sizing, alignment, and font selection.

use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight,
};
use image::{GrayImage, Luma};
use serde::{Deserialize, Serialize};

use crate::RenderError;
use crate::Result;
use crate::bitmap::LabelBitmap;

/// Text alignment within the label.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    /// Align text to the left edge.
    #[default]
    Left,
    /// Center text horizontally.
    Center,
    /// Align text to the right edge.
    Right,
}

impl TextAlign {
    /// Convert to cosmic-text's alignment type.
    fn to_cosmic(self) -> Align {
        match self {
            TextAlign::Left => Align::Left,
            TextAlign::Center => Align::Center,
            TextAlign::Right => Align::Right,
        }
    }
}

/// Text renderer backed by cosmic-text.
pub struct TextRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextRenderer {
    /// Create a new text renderer with system fonts loaded.
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }

    /// Render one or more lines of text into a [`LabelBitmap`].
    ///
    /// - `lines`: text lines to render (joined with newlines)
    /// - `print_width`: height of the tape in pixels (this becomes the bitmap
    ///   height; called "print_width" for compatibility with the C API where
    ///   the tape width is the printable width)
    /// - `font_name`: font family name to use (e.g. "DejaVu Sans")
    /// - `font_size`: explicit font size in points, or `None` for auto-detect
    /// - `font_margin`: margin in pixels on each side (top and bottom)
    /// - `align`: horizontal text alignment
    ///
    /// The renderer will:
    /// 1. If font_size is None, auto-detect the largest size that fits
    /// 2. Render the text into a buffer
    /// 3. Convert the rendered glyphs to a 1-bit bitmap
    pub fn render_text(
        &mut self,
        lines: &[&str],
        print_width: u32,
        font_name: &str,
        font_size: Option<f32>,
        font_margin: u32,
        align: TextAlign,
    ) -> Result<LabelBitmap> {
        let gray = self.render_text_gray_to_height(
            lines,
            print_width,
            font_name,
            font_size,
            font_margin,
            align,
            Weight::NORMAL.0,
            true,
        )?;
        Ok(LabelBitmap::from_gray_image(&gray, 127))
    }

    /// Render a flow element with its independent family and weight.
    pub fn render_text_weighted(
        &mut self,
        lines: &[&str],
        print_width: u32,
        font: (&str, u16),
        font_size: Option<f32>,
        font_margin: u32,
        align: TextAlign,
    ) -> Result<LabelBitmap> {
        let gray = self.render_text_gray_to_height(
            lines,
            print_width,
            font.0,
            font_size,
            font_margin,
            align,
            font.1,
            true,
        )?;
        Ok(LabelBitmap::from_gray_image(&gray, 127))
    }

    /// Render tightly bounded grayscale text for positioned composition.
    ///
    /// The grayscale coverage is intentionally retained so callers can apply
    /// anisotropic target geometry before converting to the printer's 1-bit
    /// raster.
    pub fn render_text_grayscale(
        &mut self,
        lines: &[&str],
        font_name: &str,
        font_size: f32,
        font_weight: u16,
    ) -> Result<GrayImage> {
        let height = ((font_size * 1.2).ceil() * lines.len() as f32)
            .ceil()
            .max(1.0) as u32;
        self.render_text_gray_to_height(
            lines,
            height,
            font_name,
            Some(font_size),
            0,
            TextAlign::Left,
            font_weight,
            false,
        )
    }

    /// Grayscale flow coverage with per-element typography retained.
    pub(crate) fn render_styled_flow_grayscale(
        &mut self,
        lines: &[&str],
        print_width: u32,
        font: (&str, u16),
        font_size: Option<f32>,
        font_margin: u32,
        align: TextAlign,
    ) -> Result<GrayImage> {
        self.render_text_gray_to_height(
            lines,
            print_width,
            font.0,
            font_size,
            font_margin,
            align,
            font.1,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_text_gray_to_height(
        &mut self,
        lines: &[&str],
        print_width: u32,
        font_name: &str,
        font_size: Option<f32>,
        font_margin: u32,
        align: TextAlign,
        font_weight: u16,
        center_vertically: bool,
    ) -> Result<GrayImage> {
        if print_width == 0 {
            return Err(RenderError::Text("print_width must be > 0".into()));
        }

        let text = lines.join("\n");
        if text.is_empty() {
            return Err(RenderError::Text("no text to render".into()));
        }

        let num_lines = lines.len() as f32;
        let available_height = print_width.saturating_sub(font_margin * 2) as f32;

        if available_height <= 0.0 {
            return Err(RenderError::Text(
                "font_margin too large for tape width".into(),
            ));
        }

        // Determine font size
        let font_size = font_size.unwrap_or_else(|| {
            // Auto-detect: largest size where all lines fit vertically.
            // Line height ~ font_size * 1.2, total ~ line_height * num_lines
            let size = available_height / (num_lines * 1.2);
            size.max(4.0)
        });

        let line_height = (font_size * 1.2).ceil();
        let metrics = Metrics::new(font_size, line_height);

        let family = if font_name.is_empty() {
            Family::SansSerif
        } else {
            Family::Name(font_name)
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(Weight(font_weight.clamp(1, 1000)));
        let cosmic_align = Some(align.to_cosmic());

        // We do not know the final horizontal width yet. Use a large initial
        // width so cosmic-text does not wrap, then we measure the actual
        // extent and create a tight bitmap.
        let layout_width = 16384.0f32;

        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(Some(layout_width), Some(available_height));
        buffer.set_text(&text, &attrs, Shaping::Advanced, cosmic_align);
        buffer.shape_until_scroll(&mut self.font_system, true);

        // Measure tight horizontal extent (min_x..max_x) from layout runs.
        // With Center/Right alignment, cosmic-text offsets glyphs within the
        // large layout_width. We subtract min_x so the bitmap is tight.
        let mut min_x: f32 = f32::MAX;
        let mut max_x: f32 = 0.0;
        for run in buffer.layout_runs() {
            for g in run.glyphs.iter() {
                min_x = min_x.min(g.x);
                max_x = max_x.max(g.x + g.w);
            }
        }
        if min_x == f32::MAX {
            min_x = 0.0;
        }

        let bitmap_width = ((max_x - min_x).ceil() as u32).max(1);
        let bitmap_height = print_width;
        let x_offset = min_x.floor() as i32;

        let mut bitmap = GrayImage::from_pixel(bitmap_width, bitmap_height, Luma([255]));

        // Vertical centering offset
        let total_text_height = (num_lines * line_height) as u32;
        let y_offset = if center_vertically && total_text_height < bitmap_height {
            ((bitmap_height - total_text_height) / 2) as i32
        } else {
            font_margin as i32
        };

        // Draw glyphs onto the bitmap
        let text_color = Color::rgb(0, 0, 0);
        buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            text_color,
            |x, y, w, h, color| {
                let alpha = color.a();
                let px = x - x_offset;
                let py = y + y_offset;
                for dy in 0..h as i32 {
                    for dx in 0..w as i32 {
                        let fx = px + dx;
                        let fy = py + dy;
                        if fx >= 0
                            && fy >= 0
                            && (fx as u32) < bitmap_width
                            && (fy as u32) < bitmap_height
                        {
                            let ink = 255u8.saturating_sub(alpha);
                            let pixel = bitmap.get_pixel_mut(fx as u32, fy as u32);
                            pixel.0[0] = pixel.0[0].min(ink);
                        }
                    }
                }
            },
        );

        Ok(bitmap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_align_conversion() {
        assert_eq!(TextAlign::Left.to_cosmic(), Align::Left);
        assert_eq!(TextAlign::Center.to_cosmic(), Align::Center);
        assert_eq!(TextAlign::Right.to_cosmic(), Align::Right);
    }

    #[test]
    fn test_empty_text_error() {
        let mut renderer = TextRenderer::new();
        let result = renderer.render_text(&[], 64, "sans-serif", None, 2, TextAlign::Center);
        assert!(result.is_err());
    }

    #[test]
    fn test_zero_width_error() {
        let mut renderer = TextRenderer::new();
        let result = renderer.render_text(&["hello"], 0, "sans-serif", None, 0, TextAlign::Center);
        assert!(result.is_err());
    }

    #[test]
    fn test_render_basic_text() {
        let mut renderer = TextRenderer::new();
        // Use a generic family so it works even without specific fonts
        let result = renderer.render_text(&["Test"], 64, "", Some(24.0), 2, TextAlign::Left);
        // This may succeed or fail depending on available system fonts.
        // We just verify it does not panic.
        if let Ok(bmp) = result {
            assert!(bmp.width() > 0);
            assert_eq!(bmp.height(), 64);
        }
    }
}

// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Semantic QR-code rendering for PTL elements.

use qrcode::{Color, EcLevel, QrCode};

use crate::bitmap::LabelBitmap;
use crate::document::QrErrorCorrection;
use crate::{RenderError, Result};

const QUIET_ZONE_MODULES: u32 = 4;

/// Encode `content` and render square modules into a `size`-pixel canvas.
///
/// The largest whole-number module size that fits is used. Any remaining
/// pixels are added outside the required four-module quiet zone, keeping every
/// QR module square and pixel exact.
pub fn render_qr(
    content: &str,
    error_correction: QrErrorCorrection,
    size: u32,
    min_module_size: u32,
) -> Result<LabelBitmap> {
    if content.is_empty() {
        return Err(RenderError::Layout(
            "QR code content cannot be empty".to_string(),
        ));
    }
    if size == 0 {
        return Err(RenderError::Layout(
            "QR code size must be greater than zero".to_string(),
        ));
    }
    if min_module_size == 0 {
        return Err(RenderError::Layout(
            "QR code min_module_size must be greater than zero".to_string(),
        ));
    }

    let code = QrCode::with_error_correction_level(content.as_bytes(), error_correction.into())
        .map_err(|error| RenderError::Layout(format!("QR code encoding failed: {error}")))?;
    let symbol_width = u32::try_from(code.width())
        .map_err(|_| RenderError::Layout("QR code dimensions are too large".to_string()))?;
    let total_modules = symbol_width + QUIET_ZONE_MODULES * 2;
    let module_size = size / total_modules;
    if module_size < min_module_size {
        let required_size = total_modules * min_module_size;
        return Err(RenderError::Layout(format!(
            "QR code requires at least {required_size} pixels for its symbol, quiet zone, and min_module_size {min_module_size}, but size is {size}"
        )));
    }

    let rendered_size = total_modules * module_size;
    let outer_padding = (size - rendered_size) / 2;
    let symbol_origin = outer_padding + QUIET_ZONE_MODULES * module_size;
    let mut bitmap = LabelBitmap::new(size, size);

    for module_y in 0..symbol_width {
        for module_x in 0..symbol_width {
            if code[(module_x as usize, module_y as usize)] != Color::Dark {
                continue;
            }
            let pixel_x = symbol_origin + module_x * module_size;
            let pixel_y = symbol_origin + module_y * module_size;
            for y in pixel_y..pixel_y + module_size {
                for x in pixel_x..pixel_x + module_size {
                    bitmap.set_pixel(x, y, true);
                }
            }
        }
    }

    Ok(bitmap)
}

impl From<QrErrorCorrection> for EcLevel {
    fn from(value: QrErrorCorrection) -> Self {
        match value {
            QrErrorCorrection::Low => Self::L,
            QrErrorCorrection::Medium => Self::M,
            QrErrorCorrection::Quartile => Self::Q,
            QrErrorCorrection::High => Self::H,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_modules_and_quiet_zone_use_whole_pixels() {
        let bitmap = render_qr("HELLO", QrErrorCorrection::Low, 58, 2).unwrap();

        assert_eq!((bitmap.width(), bitmap.height()), (58, 58));
        // Version 1 is 21 modules wide. With a four-module quiet zone, a
        // 58-pixel canvas yields exactly two pixels per module.
        assert!(!bitmap.get_pixel(7, 7));
        assert!(bitmap.get_pixel(8, 8));
        assert!(bitmap.get_pixel(9, 9));
        assert!(!bitmap.get_pixel(10, 10));
    }

    #[test]
    fn qr_rejects_a_canvas_smaller_than_its_module_matrix() {
        let error = render_qr("HELLO", QrErrorCorrection::Low, 28, 1).unwrap_err();
        assert!(error.to_string().contains("requires at least 29 pixels"));
    }

    #[test]
    fn qr_enforces_the_requested_minimum_module_size() {
        let error = render_qr("HELLO", QrErrorCorrection::Low, 58, 3).unwrap_err();
        assert!(error.to_string().contains("requires at least 87 pixels"));
    }
}

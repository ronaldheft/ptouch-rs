// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Semantic QR-code rendering for PTL elements.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use qrcode::{Color, EcLevel, QrCode};
use qrcodegen::{Mask, QrCode as SourceQrCode, QrCodeEcc, Version};

use crate::bitmap::LabelBitmap;
use crate::document::{QrErrorCorrection, QrSource};
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
    source: Option<&QrSource>,
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

    let (symbol_width, modules) = if let Some(source) = source {
        source_modules(source, error_correction)?
    } else {
        let code = QrCode::with_error_correction_level(content.as_bytes(), error_correction.into())
            .map_err(|error| RenderError::Layout(format!("QR code encoding failed: {error}")))?;
        let width = u32::try_from(code.width())
            .map_err(|_| RenderError::Layout("QR code dimensions are too large".to_string()))?;
        let modules = code
            .into_colors()
            .into_iter()
            .map(|color| color == Color::Dark)
            .collect();
        (width, modules)
    };
    let total_modules = symbol_width + QUIET_ZONE_MODULES * 2;
    let module_size = size / total_modules;
    if module_size < min_module_size {
        let required_size = total_modules.checked_mul(min_module_size).ok_or_else(|| {
            RenderError::Layout("QR code minimum module size is too large".to_string())
        })?;
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
            let index = (module_y * symbol_width + module_x) as usize;
            if !modules[index] {
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

fn source_modules(
    source: &QrSource,
    error_correction: QrErrorCorrection,
) -> Result<(u32, Vec<bool>)> {
    if !(1..=40).contains(&source.version) {
        return Err(RenderError::Layout(
            "QR source version must be between 1 and 40".to_string(),
        ));
    }
    if source.mask_pattern > 7 {
        return Err(RenderError::Layout(
            "QR source mask_pattern must be between 0 and 7".to_string(),
        ));
    }
    let data_codewords = BASE64_STANDARD
        .decode(&source.data_codewords_base64)
        .map_err(|error| {
            RenderError::Layout(format!(
                "QR source data_codewords_base64 is invalid: {error}"
            ))
        })?;
    if data_codewords.is_empty() {
        return Err(RenderError::Layout(
            "QR source data_codewords_base64 cannot be empty".to_string(),
        ));
    }

    let version = Version::new(source.version);
    let mask = Mask::new(source.mask_pattern);
    let code = std::panic::catch_unwind(|| {
        SourceQrCode::encode_codewords(
            version,
            error_correction.into(),
            &data_codewords,
            Some(mask),
        )
    })
    .map_err(|_| {
        RenderError::Layout(format!(
            "QR source data has {} codewords, which does not match version {} at error correction {}",
            data_codewords.len(),
            source.version,
            error_correction.as_str().to_uppercase(),
        ))
    })?;
    let width = u32::try_from(code.size())
        .map_err(|_| RenderError::Layout("QR code dimensions are too large".to_string()))?;
    let mut modules = Vec::with_capacity((code.size() * code.size()) as usize);
    for y in 0..code.size() {
        for x in 0..code.size() {
            modules.push(code.get_module(x, y));
        }
    }
    Ok((width, modules))
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

impl From<QrErrorCorrection> for QrCodeEcc {
    fn from(value: QrErrorCorrection) -> Self {
        match value {
            QrErrorCorrection::Low => Self::Low,
            QrErrorCorrection::Medium => Self::Medium,
            QrErrorCorrection::Quartile => Self::Quartile,
            QrErrorCorrection::High => Self::High,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_modules_and_quiet_zone_use_whole_pixels() {
        let bitmap = render_qr("HELLO", QrErrorCorrection::Low, 58, 2, None).unwrap();

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
        let error = render_qr("HELLO", QrErrorCorrection::Low, 28, 1, None).unwrap_err();
        assert!(error.to_string().contains("requires at least 29 pixels"));
    }

    #[test]
    fn qr_enforces_the_requested_minimum_module_size() {
        let error = render_qr("HELLO", QrErrorCorrection::Low, 58, 3, None).unwrap_err();
        assert!(error.to_string().contains("requires at least 87 pixels"));
    }

    #[test]
    fn qr_rejects_a_minimum_module_size_that_overflows() {
        let error = render_qr("HELLO", QrErrorCorrection::Low, 58, u32::MAX, None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("minimum module size is too large")
        );
    }

    #[test]
    fn qr_can_preserve_source_codewords_version_and_mask() {
        let source = QrSource {
            data_codewords_base64: "QFSEVMTE8OwR7BHsEewR7BHsEQ==".to_string(),
            version: 1,
            mask_pattern: 7,
        };
        let preserved = render_qr("HELLO", QrErrorCorrection::Low, 58, 2, Some(&source)).unwrap();
        let automatic = render_qr("HELLO", QrErrorCorrection::Low, 58, 2, None).unwrap();

        let differences = (0..58)
            .flat_map(|y| (0..58).map(move |x| (x, y)))
            .filter(|&(x, y)| preserved.get_pixel(x, y) != automatic.get_pixel(x, y))
            .count();
        assert!(
            differences > 0,
            "fixed source mask should differ from automatic encoding"
        );
        assert!(preserved.get_pixel(8, 8));
        assert!(!preserved.get_pixel(7, 7));
    }

    #[test]
    fn qr_rejects_invalid_source_metadata() {
        let source = QrSource {
            data_codewords_base64: "not base64".to_string(),
            version: 1,
            mask_pattern: 7,
        };
        let error = render_qr("HELLO", QrErrorCorrection::Low, 58, 2, Some(&source)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("data_codewords_base64 is invalid")
        );
    }
}

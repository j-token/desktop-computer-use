use std::fs::File;
use std::io::BufWriter;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::SystemTime;

use dcu_core::Error;

#[derive(Clone)]
pub struct RgbaImage {
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
    pub captured_at: SystemTime,
    pub frame_sequence: u64,
    pub source_frame_sequence: u64,
    pub frame_timestamp_ns: i64,
    pub frame_generation: u64,
    pub fresh_frame: bool,
    pub cached_frame: bool,
}

impl RgbaImage {
    pub fn new(width: i32, height: i32, rgba: Vec<u8>) -> Self {
        Self {
            width,
            height,
            rgba,
            captured_at: SystemTime::now(),
            frame_sequence: 0,
            source_frame_sequence: 0,
            frame_timestamp_ns: 0,
            frame_generation: 0,
            fresh_frame: false,
            cached_frame: false,
        }
    }
}

/// Nearest-neighbour resampling.
pub fn resize(input: &RgbaImage, width: i32, height: i32) -> RgbaImage {
    if input.width == width && input.height == height {
        return RgbaImage::new(width, height, input.rgba.clone());
    }
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height {
        let source_y = (input.height - 1).min(y * input.height / height);
        for x in 0..width {
            let source_x = (input.width - 1).min(x * input.width / width);
            let source = (source_y as usize * input.width as usize + source_x as usize) * 4;
            let target = (y as usize * width as usize + x as usize) * 4;
            rgba[target..target + 4].copy_from_slice(&input.rgba[source..source + 4]);
        }
    }
    RgbaImage::new(width, height, rgba)
}

/// Exact 2x2 box average. An odd trailing row or column is dropped, so output
/// pixel (x, y) covers source pixels [2x, 2x + 1] x [2y, 2y + 1].
pub fn downscale_half(input: &RgbaImage) -> RgbaImage {
    let width = (input.width / 2).max(1);
    let height = (input.height / 2).max(1);
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    let at = |x: i32, y: i32, channel: usize| {
        u32::from(input.rgba[(y as usize * input.width as usize + x as usize) * 4 + channel])
    };
    for y in 0..height {
        let y0 = (input.height - 1).min(y * 2);
        let y1 = (input.height - 1).min(y * 2 + 1);
        for x in 0..width {
            let x0 = (input.width - 1).min(x * 2);
            let x1 = (input.width - 1).min(x * 2 + 1);
            let target = (y as usize * width as usize + x as usize) * 4;
            for channel in 0..4 {
                let sum = at(x0, y0, channel)
                    + at(x1, y0, channel)
                    + at(x0, y1, channel)
                    + at(x1, y1, channel);
                rgba[target + channel] = ((sum + 2) / 4) as u8;
            }
        }
    }
    RgbaImage::new(width, height, rgba)
}

/// Writes the image as `format` and returns its MIME type. The file is
/// readable only by the user, since it shows the desktop.
pub fn save(
    image: &RgbaImage,
    path: &Path,
    format: &str,
    quality: i32,
) -> Result<&'static str, Error> {
    let mime = match format {
        "png" => {
            write_png(image, path)?;
            "image/png"
        }
        "jpeg" | "jpg" => {
            write_jpeg(image, path, quality)?;
            "image/jpeg"
        }
        _ => return Err(Error::invalid_argument("format must be jpeg or png")),
    };
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(mime)
}

fn create_file(path: &Path) -> Result<BufWriter<File>, Error> {
    File::create(path)
        .map(BufWriter::new)
        .map_err(|_| Error::new("capture_unavailable", "Could not create screenshot file"))
}

fn write_png(image: &RgbaImage, path: &Path) -> Result<(), Error> {
    let file = create_file(path)?;
    let failed = |_| Error::new("capture_unavailable", "PNG encoding failed");
    let mut encoder = png::Encoder::new(file, image.width as u32, image.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(failed)?;
    writer.write_image_data(&image.rgba).map_err(failed)?;
    writer.finish().map_err(failed)
}

fn write_jpeg(image: &RgbaImage, path: &Path, quality: i32) -> Result<(), Error> {
    let failed = || Error::new("capture_unavailable", "JPEG encoding failed");
    let width = u16::try_from(image.width).map_err(|_| failed())?;
    let height = u16::try_from(image.height).map_err(|_| failed())?;
    let file = create_file(path)?;
    let mut encoder = jpeg_encoder::Encoder::new(file, quality.clamp(1, 100) as u8);
    // libjpeg's default chroma subsampling.
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
    encoder
        .encode(&image.rgba, width, height, jpeg_encoder::ColorType::Rgba)
        .map_err(|_| failed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_scale_averages_pixels_and_drops_odd_trailing_column() {
        let image = RgbaImage::new(
            3,
            2,
            vec![
                0, 0, 0, 255, 4, 8, 12, 255, 255, 255, 255, 255, 8, 16, 24, 255, 12, 24, 36, 255,
                255, 255, 255, 255,
            ],
        );

        let scaled = downscale_half(&image);

        assert_eq!((scaled.width, scaled.height), (1, 1));
        assert_eq!(scaled.rgba, vec![6, 12, 18, 255]);
    }

    #[test]
    fn resize_picks_nearest_source_pixels() {
        let image = RgbaImage::new(2, 1, vec![1, 1, 1, 255, 9, 9, 9, 255]);

        let resized = resize(&image, 4, 1);

        assert_eq!(
            resized.rgba,
            vec![1, 1, 1, 255, 1, 1, 1, 255, 9, 9, 9, 255, 9, 9, 9, 255]
        );
    }

    #[test]
    fn unknown_format_is_rejected_before_writing() {
        let image = RgbaImage::new(1, 1, vec![0, 0, 0, 255]);
        let path = std::env::temp_dir().join("dcu-linux-unknown-format-test.gif");

        let error = save(&image, &path, "gif", 85).unwrap_err();

        assert_eq!(error.message, "format must be jpeg or png");
        assert!(!path.exists());
    }
}

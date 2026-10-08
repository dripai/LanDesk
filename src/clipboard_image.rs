use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageDecoder, Limits, codecs::png::PngDecoder};
use std::{
    io::Cursor,
    time::{Duration, Instant},
};

pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub const IMAGE_CHUNK_BYTES: usize = 64 * 1024;
const MAX_PIXELS: u64 = 16_000_000;

pub struct Transfer {
    id: u32,
    total: usize,
    data: Vec<u8>,
    updated: Instant,
}

#[derive(Default)]
pub struct Images(Option<Transfer>);
impl Images {
    pub fn expire(&mut self) {
        if self
            .0
            .as_ref()
            .is_some_and(|value| value.updated.elapsed() > Duration::from_secs(30))
        {
            self.0 = None;
        }
    }
    pub fn cancel(&mut self, id: u32) {
        if self.0.as_ref().is_some_and(|value| value.id == id) {
            self.0 = None;
        }
    }
    pub fn chunk(
        &mut self,
        id: u32,
        offset: usize,
        total: usize,
        encoded: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.expire();
        let result = self.append(id, offset, total, encoded);
        if result.is_err() {
            self.0 = None;
        }
        result
    }
    fn append(
        &mut self,
        id: u32,
        offset: usize,
        total: usize,
        encoded: &str,
    ) -> Result<Option<Vec<u8>>> {
        ensure!(total > 0 && total <= MAX_IMAGE_BYTES, "图片不能超过 10 MiB");
        ensure!(
            encoded.len() <= IMAGE_CHUNK_BYTES.div_ceil(3) * 4,
            "图片分块过大"
        );
        let bytes = STANDARD.decode(encoded).context("图片分块编码无效")?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= IMAGE_CHUNK_BYTES,
            "图片分块大小无效"
        );
        if self.0.is_none() {
            ensure!(offset == 0, "图片传输已过期，请重新粘贴");
            self.0 = Some(Transfer {
                id,
                total,
                data: Vec::with_capacity(total),
                updated: Instant::now(),
            });
        }
        let value = self.0.as_mut().unwrap();
        ensure!(
            value.id == id && value.total == total && value.data.len() == offset,
            "图片分块顺序不一致"
        );
        ensure!(
            bytes.len() <= total - value.data.len(),
            "图片数据超过声明大小"
        );
        value.data.extend_from_slice(&bytes);
        value.updated = Instant::now();
        Ok(if value.data.len() == total {
            self.0.take().map(|v| v.data)
        } else {
            None
        })
    }
}

pub fn validate_png(data: Vec<u8>) -> Result<Vec<u8>> {
    ensure!(
        !data.is_empty() && data.len() <= MAX_IMAGE_BYTES,
        "图片不能超过 10 MiB"
    );
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    let decoder =
        PngDecoder::with_limits(Cursor::new(&data), limits).context("剪贴板图片不是有效 PNG")?;
    let (width, height) = decoder.dimensions();
    ensure!(
        u64::from(width) * u64::from(height) <= MAX_PIXELS,
        "图片不能超过 1600 万像素"
    );
    ensure!(
        decoder.total_bytes() <= 128 * 1024 * 1024,
        "图片解码内存过大"
    );
    let mut pixels = vec![0; decoder.total_bytes() as usize];
    decoder
        .read_image(&mut pixels)
        .context("PNG 图片内容损坏")?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunks_require_matching_identity_offset_and_size() {
        let mut images = Images::default();
        assert!(
            images
                .chunk(1, 0, 4, &STANDARD.encode([1, 2]))
                .unwrap()
                .is_none()
        );
        assert!(images.chunk(1, 1, 4, &STANDARD.encode([3, 4])).is_err());
        assert!(images.0.is_none());
        assert!(
            images
                .chunk(1, 0, 4, &STANDARD.encode([1, 2]))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            images
                .chunk(1, 2, 4, &STANDARD.encode([3, 4]))
                .unwrap()
                .unwrap(),
            [1, 2, 3, 4]
        );
        assert!(images.0.is_none());
        assert!(images.chunk(1, 0, MAX_IMAGE_BYTES + 1, "AA==").is_err());
        assert!(images.chunk(1, 0, 1, "%%%%").is_err());
        assert!(images.chunk(1, 0, 1, "AAAA").is_err());
    }
    #[test]
    fn cancel_and_expiry_drop_partial_images() {
        let mut images = Images::default();
        images.chunk(1, 0, 4, "AQI=").unwrap();
        images.cancel(2);
        assert!(images.0.is_some());
        images.cancel(1);
        assert!(images.0.is_none());
        images.chunk(2, 0, 4, "AQI=").unwrap();
        images.0.as_mut().unwrap().updated = Instant::now() - Duration::from_secs(31);
        images.expire();
        assert!(images.0.is_none());
    }
    #[test]
    fn valid_png_round_trip_and_corrupt_image_rejection() {
        let mut data = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        let png = data.into_inner();
        assert_eq!(validate_png(png.clone()).unwrap(), png);
        assert!(validate_png(png[..20].to_vec()).is_err());
        assert!(validate_png(b"not an image".to_vec()).is_err());
    }
}

//! Avatars of users, projects and groups (M6): an uploaded PNG, JPEG, GIF or
//! WebP is decoded with size limits, center-cropped to a square, scaled to
//! 256 px and stored as PNG in the blob store. Without one, the web UI shows
//! initials (it knows the theme's colors); requests for a missing avatar get 404.

use std::io::Cursor;

use bytes::Bytes;
use image::{ImageFormat, ImageReader, Limits, imageops::FilterType};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Largest upload accepted.
pub const MAX_UPLOAD: usize = 5 << 20;
const SIZE: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    User(i64),
    Project(i64),
    Group(i64),
}

impl Owner {
    fn key(self) -> String {
        match self {
            Owner::User(id) => format!("avatars/user/{id}.png"),
            Owner::Project(id) => format!("avatars/project/{id}.png"),
            Owner::Group(id) => format!("avatars/group/{id}.png"),
        }
    }
}

/// An upload → a 256 px square PNG.
pub fn process(data: &[u8]) -> ApiResult<Vec<u8>> {
    if data.is_empty() || data.len() > MAX_UPLOAD {
        return Err(ApiError::bad("an avatar is an image of at most 5 MB"));
    }
    let format = image::guess_format(data).map_err(|_| ApiError::bad("use a PNG, JPEG, GIF or WebP image"))?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP) {
        return Err(ApiError::bad("use a PNG, JPEG, GIF or WebP image"));
    }
    let mut reader = ImageReader::with_format(Cursor::new(data), format);
    // a small file can still claim a huge canvas: bound what decoding may use
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 << 20);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| ApiError::bad(format!("the image could not be read: {e}")))?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err(ApiError::bad("the image is empty"));
    }
    let side = w.min(h);
    let square = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let scaled = if side > SIZE { square.resize_exact(SIZE, SIZE, FilterType::Lanczos3) } else { square };
    let mut out = Vec::new();
    scaled.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).map_err(|e| ApiError::Internal(e.into()))?;
    Ok(out)
}

/// Store a processed avatar and note when (the version clients cache by).
pub async fn set(state: &AppState, owner: Owner, data: &[u8]) -> ApiResult<()> {
    let png = process(data)?;
    state.blobs.put_bytes(&owner.key(), Bytes::from(png)).await.map_err(ApiError::Internal)?;
    stamp(state, owner, true).await
}

pub async fn remove(state: &AppState, owner: Owner) -> ApiResult<()> {
    stamp(state, owner, false).await?;
    if let Err(e) = state.blobs.delete(&owner.key()).await {
        tracing::warn!(error = %e, "avatar blob not removed");
    }
    Ok(())
}

async fn stamp(state: &AppState, owner: Owner, present: bool) -> ApiResult<()> {
    match owner {
        Owner::User(id) => forgust_db::users::set_avatar(&state.db, id, present).await?,
        Owner::Project(id) => forgust_db::projects::set_avatar(&state.db, id, present).await?,
        Owner::Group(id) => forgust_db::namespaces::set_avatar(&state.db, id, present).await?,
    }
    Ok(())
}

/// The stored PNG, if any.
pub async fn get(state: &AppState, owner: Owner) -> ApiResult<Option<Vec<u8>>> {
    use futures::TryStreamExt;
    let Some((_, stream)) = state.blobs.get(&owner.key()).await.map_err(ApiError::Internal)? else { return Ok(None) };
    let chunks: Vec<Bytes> = stream.try_collect().await.map_err(|e| ApiError::Internal(e.into()))?;
    Ok(Some(chunks.concat()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn crops_and_scales_to_a_square() {
        let out = process(&png(800, 400)).unwrap();
        let back = image::load_from_memory(&out).unwrap();
        assert_eq!((back.width(), back.height()), (256, 256));
        // small images keep their size (as a square)
        let small = image::load_from_memory(&process(&png(64, 100)).unwrap()).unwrap();
        assert_eq!((small.width(), small.height()), (64, 64));
    }

    #[test]
    fn refuses_what_is_not_an_image() {
        assert!(process(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
        assert!(process(b"").is_err());
        assert!(process(&vec![0u8; MAX_UPLOAD + 1]).is_err());
    }
}

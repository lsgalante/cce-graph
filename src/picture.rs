//! A decoded picture and the GPU image it is drawn as: the editor's loaded
//! images and the vault graph's thumbnails both.

use image::GenericImageView;

/// Decode an image to RGBA8, shrunk to fit `max_dim` (never enlarged), as
/// (pixels, width, height).
pub fn decode(path: &std::path::Path, max_dim: u32) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::open(path).ok()?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let scale = (max_dim as f32 / w.max(h) as f32).min(1.0);
    // At least a pixel each way: at an extreme aspect ratio the short side
    // rounds to 0, the aspect ratio goes infinite, and the size saved with
    // it is not KDL.
    let (nw, nh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
    let img = if (nw, nh) == (w, h) { img } else { img.resize_exact(nw, nh, image::imageops::FilterType::Triangle) };
    Some((img.to_rgba8().into_raw(), nw, nh))
}

pub struct Picture {
    /// RGBA8, `width` x `height`: kept to upload again if the renderer is
    /// rebuilt (its image ids die with it).
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The GPU image and the renderer epoch it was uploaded in; uploaded at
    /// first draw, freed on drop.
    texture: Option<(u32, u32)>,
}

impl Picture {
    pub fn new((pixels, width, height): (Vec<u8>, u32, u32)) -> Picture {
        Picture { pixels, width, height, texture: None }
    }

    /// Height over width.
    pub fn aspect(&self) -> f32 {
        self.height as f32 / self.width as f32
    }

    /// The image id to draw by, uploading the pixels when this renderer has
    /// not got them yet.
    pub fn texture_id(&mut self) -> u32 {
        let epoch = cce_ui::draw::renderer_epoch();
        match self.texture {
            Some((id, e)) if e == epoch => id,
            _ => {
                let id = cce_ui::draw::upload_rgba_mipmapped(self.pixels.clone(), self.width, self.height);
                self.texture = Some((id, epoch));
                id
            }
        }
    }
}

impl Drop for Picture {
    fn drop(&mut self) {
        // An id from an earlier renderer named nothing once it went.
        if let Some((id, epoch)) = self.texture {
            if epoch == cce_ui::draw::renderer_epoch() {
                cce_ui::draw::free_image(id);
            }
        }
    }
}

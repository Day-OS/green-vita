use std::sync::Arc;
use std::sync::OnceLock;

pub struct TitleImage {
    rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    texture: OnceLock<egui::TextureHandle>,
}

impl TitleImage {
    pub fn new(rgba: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            rgba,
            width,
            height,
            texture: OnceLock::new(),
        }
    }

    pub fn texture(&self, ctx: &egui::Context, label: &'static str) -> &egui::TextureHandle {
        self.texture.get_or_init(|| {
            ctx.load_texture(
                label,
                egui::ColorImage::from_rgba_unmultiplied(
                    [self.width as usize, self.height as usize],
                    &self.rgba,
                ),
                egui::TextureOptions::LINEAR,
            )
        })
    }
}

const XBOX_LOGO_PNG: &[u8] = include_bytes!("../../assets/xbox-logo-white.png");

pub(super) fn load_bundled_logo() -> Arc<TitleImage> {
    let image =
        image::load_from_memory(XBOX_LOGO_PNG).expect("bundled Xbox logo PNG failed to decode");
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Arc::new(TitleImage::new(rgba.into_raw(), width, height))
}

/// Zeroes the alpha outside the largest circle that fits, for a circular Xbox avatar.
pub(super) fn mask_to_circle(image: &mut TitleImage) {
    let (width, height) = (image.width as f32, image.height as f32);
    let (cx, cy) = (width / 2.0, height / 2.0);
    let radius = cx.min(cy);
    for y in 0..image.height {
        for x in 0..image.width {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            if dx * dx + dy * dy > radius * radius {
                let alpha_index = ((y * image.width + x) * 4 + 3) as usize;
                image.rgba[alpha_index] = 0;
            }
        }
    }
}

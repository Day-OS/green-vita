use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet, VecDeque};

const MAX_NEW_COLOR_TEXTURES_PER_FRAME: usize = 1;

#[derive(Default)]
pub struct SdlEguiPainter {
    textures: HashMap<egui::TextureId, SdlEguiTexture>,
    pending_textures: HashMap<egui::TextureId, egui::epaint::ImageDelta>,
    pending_order: VecDeque<egui::TextureId>,
    vertices: Vec<sdl2::render::Vertex>,
}

struct SdlEguiTexture {
    texture: sdl2::render::Texture,
    uv_scale: egui::Vec2,
}

impl SdlEguiPainter {
    pub fn paint(
        &mut self,
        canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
        screen_size: [u32; 2],
        pixels_per_point: f32,
        primitives: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
    ) -> Result<()> {
        self.apply_textures(canvas, primitives, textures_delta)?;

        for clipped_primitive in primitives {
            let Some(clip_rect) =
                Self::sdl_clip_rect(clipped_primitive.clip_rect, screen_size, pixels_per_point)
            else {
                continue;
            };
            canvas.set_clip_rect(clip_rect);

            let egui::epaint::Primitive::Mesh(mesh) = &clipped_primitive.primitive else {
                continue;
            };
            if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                continue;
            }

            self.vertices.clear();
            let Some(texture) = self.textures.get(&mesh.texture_id) else {
                // A catalog image can wait in the upload queue for a later frame. Drawing it
                // without a texture produces an opaque rectangle, so leave its space empty.
                continue;
            };
            let uv_scale = texture.uv_scale;
            self.vertices.extend(
                mesh.vertices
                    .iter()
                    .map(|vertex| Self::sdl_vertex(vertex, pixels_per_point, uv_scale)),
            );

            canvas
                .render_geometry(&self.vertices, Some(&texture.texture), &mesh.indices)
                .map_err(anyhow::Error::msg)
                .context("failed to render egui geometry through SDL")?;
        }

        canvas.set_clip_rect(None);
        for texture_id in &textures_delta.free {
            self.textures.remove(texture_id);
            self.pending_textures.remove(texture_id);
        }
        self.pending_order
            .retain(|texture_id| self.pending_textures.contains_key(texture_id));

        Ok(())
    }

    fn apply_textures(
        &mut self,
        canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
        primitives: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
    ) -> Result<()> {
        for (texture_id, delta) in &textures_delta.set {
            let is_new_color_texture = delta.pos.is_none()
                && !self.textures.contains_key(texture_id)
                && matches!(delta.image, egui::ImageData::Color(_));
            if is_new_color_texture {
                if !self.pending_textures.contains_key(texture_id) {
                    self.pending_order.push_back(*texture_id);
                }
                self.pending_textures.insert(*texture_id, delta.clone());
                continue;
            }

            // Font atlas changes and updates to existing textures must be immediately visible.
            Self::upload_texture(canvas, &mut self.textures, *texture_id, delta)?;
        }

        let visible_texture_ids: HashSet<_> = primitives
            .iter()
            .filter_map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => Some(mesh.texture_id),
                egui::epaint::Primitive::Callback(_) => None,
            })
            .collect();

        for _ in 0..MAX_NEW_COLOR_TEXTURES_PER_FRAME {
            let Some(index) = self
                .pending_order
                .iter()
                .enumerate()
                // The title screen deliberately creates its next backdrop one frame before
                // displaying it. Prefer large textures even while they are still hidden so
                // their synchronous upload cannot consume the crossfade itself.
                .max_by_key(|(_, texture_id)| {
                    let area = self
                        .pending_textures
                        .get(texture_id)
                        .map(|delta| delta.image.width() * delta.image.height())
                        .unwrap_or(0);
                    (area, visible_texture_ids.contains(texture_id))
                })
                .map(|(index, _)| index)
            else {
                break;
            };
            let texture_id = self
                .pending_order
                .remove(index)
                .expect("pending texture index disappeared");
            let Some(delta) = self.pending_textures.remove(&texture_id) else {
                continue;
            };
            Self::upload_texture(canvas, &mut self.textures, texture_id, &delta)?;
        }

        Ok(())
    }

    fn upload_texture(
        canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
        textures: &mut HashMap<egui::TextureId, SdlEguiTexture>,
        texture_id: egui::TextureId,
        delta: &egui::epaint::ImageDelta,
    ) -> Result<()> {
        use sdl2::pixels::PixelFormatEnum;
        use sdl2::rect::Rect;
        use sdl2::render::BlendMode;

        let [width, height] = delta.image.size();
        let pixels = Self::image_to_sdl_rgba(&delta.image);

        if delta.pos.is_none() || !textures.contains_key(&texture_id) {
            let pot_width = width.next_power_of_two();
            let pot_height = height.next_power_of_two();
            let mut texture = canvas
                .create_texture_streaming(
                    PixelFormatEnum::RGBA32,
                    pot_width as u32,
                    pot_height as u32,
                )
                .map_err(anyhow::Error::msg)
                .context("failed to create SDL egui texture")?;
            texture.set_blend_mode(BlendMode::Blend);
            texture
                .update(
                    Rect::new(0, 0, width as u32, height as u32),
                    &pixels,
                    width * 4,
                )
                .map_err(anyhow::Error::msg)
                .context("failed to upload SDL egui texture")?;
            textures.insert(
                texture_id,
                SdlEguiTexture {
                    texture,
                    uv_scale: egui::vec2(
                        width as f32 / pot_width as f32,
                        height as f32 / pot_height as f32,
                    ),
                },
            );
            return Ok(());
        }

        let [x, y] = delta.pos.expect("partial texture update has a position");
        textures
            .get_mut(&texture_id)
            .context("missing SDL texture for egui partial update")?
            .texture
            .update(
                Rect::new(x as i32, y as i32, width as u32, height as u32),
                &pixels,
                width * 4,
            )
            .map_err(anyhow::Error::msg)
            .context("failed to upload SDL egui texture patch")?;
        Ok(())
    }

    fn image_to_sdl_rgba(image: &egui::ImageData) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(image.width() * image.height() * 4);
        match image {
            egui::ImageData::Color(image) => {
                for pixel in &image.pixels {
                    pixels.extend_from_slice(&pixel.to_srgba_unmultiplied());
                }
            }
            egui::ImageData::Font(image) => {
                for pixel in image.srgba_pixels(None) {
                    pixels.extend_from_slice(&pixel.to_srgba_unmultiplied());
                }
            }
        }
        pixels
    }

    fn sdl_vertex(
        vertex: &egui::epaint::Vertex,
        pixels_per_point: f32,
        uv_scale: egui::Vec2,
    ) -> sdl2::render::Vertex {
        let [r, g, b, a] = vertex.color.to_srgba_unmultiplied();
        sdl2::render::Vertex {
            position: sdl2::rect::FPoint::new(
                vertex.pos.x * pixels_per_point,
                vertex.pos.y * pixels_per_point,
            ),
            color: sdl2::pixels::Color::RGBA(r, g, b, a),
            tex_coord: sdl2::rect::FPoint::new(vertex.uv.x * uv_scale.x, vertex.uv.y * uv_scale.y),
        }
    }

    fn sdl_clip_rect(
        clip_rect: egui::Rect,
        [screen_width, screen_height]: [u32; 2],
        pixels_per_point: f32,
    ) -> Option<sdl2::rect::Rect> {
        let min_x = (clip_rect.min.x * pixels_per_point)
            .floor()
            .clamp(0.0, screen_width as f32) as i32;
        let min_y = (clip_rect.min.y * pixels_per_point)
            .floor()
            .clamp(0.0, screen_height as f32) as i32;
        let max_x = (clip_rect.max.x * pixels_per_point)
            .ceil()
            .clamp(0.0, screen_width as f32) as i32;
        let max_y = (clip_rect.max.y * pixels_per_point)
            .ceil()
            .clamp(0.0, screen_height as f32) as i32;
        let width = (max_x - min_x).max(0) as u32;
        let height = (max_y - min_y).max(0) as u32;
        if width == 0 || height == 0 {
            None
        } else {
            Some(sdl2::rect::Rect::new(min_x, min_y, width, height))
        }
    }
}

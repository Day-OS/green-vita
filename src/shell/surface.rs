use crate::app::StreamingSession;
use crate::shell::egui_painter::SdlEguiPainter;
use anyhow::{Context, Result};
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{Canvas, Texture};
use sdl2::video::Window;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub const WIDTH: u32 = 960;
pub const HEIGHT: u32 = 544;
static DMAC_UPLOAD_FALLBACK_REPORTED: AtomicBool = AtomicBool::new(false);

pub struct VitaSurface {
    canvas: Canvas<Window>,
    video_texture: Option<Texture>,
    video_width: u32,
    video_height: u32,
    last_frame_id: u64,
    egui_painter: SdlEguiPainter,
}

impl VitaSurface {
    pub fn new(video: &sdl2::VideoSubsystem) -> Result<Self> {
        let window = video
            .window("GreenVita", WIDTH, HEIGHT)
            .position_centered()
            .build()
            .context("failed to create SDL Vita window")?;
        let mut canvas = window
            .into_canvas()
            .accelerated()
            .present_vsync()
            .build()
            .map_err(anyhow::Error::msg)
            .context("failed to create SDL Vita renderer")?;
        canvas
            .set_logical_size(WIDTH, HEIGHT)
            .map_err(anyhow::Error::msg)
            .context("failed to set Vita logical render size")?;

        Ok(Self {
            canvas,
            video_texture: None,
            video_width: 0,
            video_height: 0,
            last_frame_id: 0,
            egui_painter: SdlEguiPainter::default(),
        })
    }

    /// Where the video quad lands on screen - accounts for letterboxing, see `fit_rect`.
    pub fn video_rect(&self) -> sdl2::rect::Rect {
        Self::fit_rect(self.video_width, self.video_height, WIDTH, HEIGHT)
    }

    pub fn upload_video_frame(&mut self, streaming: Option<&StreamingSession>) -> Result<()> {
        let Some((frame_id, frame)) = streaming.and_then(StreamingSession::video_frame) else {
            return Ok(());
        };
        if frame_id == self.last_frame_id {
            return Ok(());
        }
        let pixels = &frame.pixels;
        let width = frame.width;
        let height = frame.height;
        let pitch = frame.pitch;

        if self.video_texture.is_none() || self.video_width != width || self.video_height != height
        {
            // AVCDEC's RGBA565 byte layout maps to the Vita GXM U5U6U5 BGR texture path.
            let texture = self
                .canvas
                .create_texture_streaming(PixelFormatEnum::BGR565, width, height)
                .map_err(anyhow::Error::msg)
                .context("failed to create SDL BGR565 video texture")?;
            self.video_texture = Some(texture);
            self.video_width = width;
            self.video_height = height;
        }

        let texture = self.video_texture.as_mut().expect("texture was created");
        let upload_started_at = Instant::now();
        let frame_len = (pitch as usize)
            .checked_mul(height as usize)
            .context("video frame length overflow")?;
        let dmac_result = texture
            .with_lock(None, |destination, destination_pitch| {
                if destination_pitch != pitch as usize
                    || destination.len() < frame_len
                    || pixels.len() < frame_len
                {
                    return None;
                }
                Some(unsafe {
                    vitasdk_sys::sceDmacMemcpy(
                        destination.as_mut_ptr().cast(),
                        pixels.as_ptr().cast(),
                        frame_len as u32,
                    )
                })
            })
            .map_err(anyhow::Error::msg)
            .context("failed to lock SDL BGR565 video texture")?;
        if !dmac_result.is_some_and(|ret| ret >= 0) {
            if let Some(ret) = dmac_result
                && !DMAC_UPLOAD_FALLBACK_REPORTED.swap(true, Ordering::Relaxed)
            {
                eprintln!("sceDmacMemcpy texture upload failed ({ret:#x}); using SDL fallback");
            }
            texture
                .update(None, pixels, pitch as usize)
                .map_err(anyhow::Error::msg)
                .context("failed to upload SDL BGR565 video frame")?;
        }
        crate::streaming::video::record_video_upload(upload_started_at.elapsed());
        self.last_frame_id = frame_id;
        Ok(())
    }

    pub fn draw_scene(&mut self, show_video: bool) -> Result<()> {
        self.canvas.set_draw_color(sdl2::pixels::Color::BLACK);
        self.canvas.clear();

        if show_video && let Some(texture) = self.video_texture.as_ref() {
            let destination = self.video_rect();
            self.canvas
                .copy(texture, None, destination)
                .map_err(anyhow::Error::msg)
                .context("failed to draw SDL YUV video frame")?;
        }

        Ok(())
    }

    pub fn paint_egui(
        &mut self,
        pixels_per_point: f32,
        primitives: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
    ) -> Result<()> {
        self.egui_painter.paint(
            &mut self.canvas,
            [WIDTH, HEIGHT],
            pixels_per_point,
            primitives,
            textures_delta,
        )?;
        self.canvas.present();
        Ok(())
    }

    fn fit_rect(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> sdl2::rect::Rect {
        if src_w == 0 || src_h == 0 {
            return sdl2::rect::Rect::new(0, 0, dst_w, dst_h);
        }
        let src_aspect = src_w as f32 / src_h as f32;
        let dst_aspect = dst_w as f32 / dst_h as f32;
        if src_aspect > dst_aspect {
            let height = (dst_w as f32 / src_aspect).round() as u32;
            let y = ((dst_h - height) / 2) as i32;
            sdl2::rect::Rect::new(0, y, dst_w, height)
        } else {
            let width = (dst_h as f32 * src_aspect).round() as u32;
            let x = ((dst_w - width) / 2) as i32;
            sdl2::rect::Rect::new(x, 0, width, dst_h)
        }
    }
}

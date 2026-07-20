use crate::app::StreamingSession;
use crate::shell::egui_painter::SdlEguiPainter;
use crate::streaming::video::{DirectVideoOutput, VideoTextureTarget};
use anyhow::{Context, Result};
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{Canvas, Texture};
use sdl2::video::Window;
use std::sync::Arc;

pub const WIDTH: u32 = 960;
pub const HEIGHT: u32 = 544;

pub struct VitaSurface {
    canvas: Canvas<Window>,
    video_textures: Option<[Texture; 2]>,
    displayed_video_texture: Option<usize>,
    direct_video_output: Option<Arc<DirectVideoOutput>>,
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
            .build()
            .map_err(anyhow::Error::msg)
            .context("failed to create SDL Vita renderer")?;
        canvas
            .set_logical_size(WIDTH, HEIGHT)
            .map_err(anyhow::Error::msg)
            .context("failed to set Vita logical render size")?;

        Ok(Self {
            canvas,
            video_textures: None,
            displayed_video_texture: None,
            direct_video_output: None,
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

    pub fn window(&self) -> &Window {
        self.canvas.window()
    }

    pub fn sync_video_frame(&mut self, streaming: Option<&StreamingSession>) -> Result<()> {
        let Some(streaming) = streaming else {
            self.detach_direct_video_output();
            return Ok(());
        };
        self.ensure_direct_video_output(streaming)?;

        let Some((frame_id, frame)) = streaming.video_frame() else {
            return Ok(());
        };
        if frame_id == self.last_frame_id {
            return Ok(());
        }
        let width = frame.width;
        let height = frame.height;
        let index = frame.texture_index;
        if index >= 2 {
            anyhow::bail!("decoder returned invalid direct texture index {index}");
        }
        if let Some(output) = &self.direct_video_output {
            output.mark_displayed(index);
        }
        self.displayed_video_texture = Some(index);
        self.video_width = width;
        self.video_height = height;
        crate::streaming::video::record_video_presented();
        self.last_frame_id = frame_id;
        Ok(())
    }

    fn ensure_direct_video_output(&mut self, streaming: &StreamingSession) -> Result<()> {
        let output = streaming.direct_video_output();
        let output_is_current = self
            .direct_video_output
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &output));
        if !output_is_current && self.direct_video_output.is_some() {
            self.detach_direct_video_output();
        }
        if !output.decoder_ready() {
            return Ok(());
        }
        if output_is_current && self.video_textures.is_some() {
            return Ok(());
        }

        self.detach_direct_video_output();
        let (width, height) = output.dimensions();
        let create_texture = || {
            self.canvas
                .create_texture_streaming(PixelFormatEnum::BGR565, width, height)
                .map_err(anyhow::Error::msg)
                .context("failed to create direct SDL BGR565 video texture")
        };
        let mut textures = [create_texture()?, create_texture()?];
        let mut targets = [
            VideoTextureTarget {
                ptr: 0,
                pitch: 0,
                capacity: 0,
            },
            VideoTextureTarget {
                ptr: 0,
                pitch: 0,
                capacity: 0,
            },
        ];
        for (index, texture) in textures.iter_mut().enumerate() {
            texture
                .with_lock(None, |pixels, pitch| {
                    targets[index] = VideoTextureTarget {
                        ptr: pixels.as_mut_ptr() as usize,
                        pitch: pitch as u32,
                        capacity: pixels.len().min(u32::MAX as usize) as u32,
                    };
                })
                .map_err(anyhow::Error::msg)
                .context("failed to lock direct SDL video texture")?;
        }
        output.set_targets(targets);
        self.video_textures = Some(textures);
        self.displayed_video_texture = None;
        self.direct_video_output = Some(output);
        self.video_width = width;
        self.video_height = height;
        self.last_frame_id = 0;
        Ok(())
    }

    fn detach_direct_video_output(&mut self) {
        if let Some(output) = self.direct_video_output.take() {
            output.clear_targets();
        }
        self.video_textures = None;
        self.displayed_video_texture = None;
        self.video_width = 0;
        self.video_height = 0;
        self.last_frame_id = 0;
    }

    pub fn draw_scene(&mut self, show_video: bool) -> Result<()> {
        self.canvas.set_draw_color(sdl2::pixels::Color::BLACK);
        self.canvas.clear();

        if show_video
            && let Some(index) = self.displayed_video_texture
            && let Some(texture) = self
                .video_textures
                .as_ref()
                .map(|textures| &textures[index])
        {
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

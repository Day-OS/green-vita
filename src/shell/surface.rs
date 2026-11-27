use crate::app::StreamingSession;
use crate::settings::ColorProfile;
use crate::shell::egui_painter::SdlEguiPainter;
use crate::streaming::video::{
    DirectVideoOutput, NUM_DISPLAY_TEXTURES, NUM_TEXTURES, VideoTextureTarget,
    memory::CdramBlock,
};
use anyhow::{Context, Result};
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{Canvas, Texture};
use sdl2::video::Window;
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub const WIDTH: u32 = 960;
pub const HEIGHT: u32 = 544;

pub struct VitaSurface {
    pub(crate) canvas: Canvas<Window>,
    video_textures: Option<[Texture; NUM_TEXTURES]>,
    displayed_video_texture: Option<usize>,
    direct_video_output: Option<Arc<DirectVideoOutput>>,
    video_width: u32,
    video_height: u32,
    last_frame_id: u64,
    egui_painter: SdlEguiPainter,
    /// 2b: keeps the CDRAM framebuffers alive for direct mode.
    cdram: Vec<CdramBlock>,
    /// 2b: bytes per pixel and framebuffer pixelformat of the direct targets.
    direct_bpp: u32,
    direct_fmt: u32,
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
            cdram: Vec::new(),
            direct_bpp: 4,
            direct_fmt: 0,
        })
    }

    /// True when a frame newer than the one currently displayed is available.
    ///
    /// Latency (2026-11-26): the Vita loop paces by the vblank counter, so a
    /// frame published just after a loop iteration waited ~16 ms for the next
    /// one (`display_pickup` median 13 ms). The renderer is created without
    /// `PRESENTVSYNC`, so presenting off-vblank is already allowed; this lets
    /// the loop break out of the vblank wait and present as soon as a frame
    /// arrives. If the panel latches at vblank this is a no-op; if it scans
    /// immediately it returns the pickup to ~1-2 ms.
    pub fn has_new_frame(&self, streaming: Option<&StreamingSession>) -> bool {
        streaming
            .and_then(|s| s.video_frame())
            .is_some_and(|(frame_id, _)| frame_id != self.last_frame_id)
    }

    /// Where the video quad lands on screen - accounts for letterboxing, see `fit_rect`.
    pub fn video_rect(&self) -> sdl2::rect::Rect {
        Self::fit_rect(self.video_width, self.video_height, WIDTH, HEIGHT)
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
        let index = frame.texture_index;
        if index >= NUM_DISPLAY_TEXTURES {
            anyhow::bail!("decoder returned invalid direct texture index {index}");
        }
        crate::streaming::video::metrics::METRICS
            .display_pickup_us
            .store(frame.published_at.elapsed().as_micros() as u64, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .display_pickup_max_us
            .fetch_max(
                frame.published_at.elapsed().as_micros() as u64,
                Ordering::Relaxed,
            );
        if let Some(output) = &self.direct_video_output {
            output.mark_displayed(index, frame.generation);
        }
        self.displayed_video_texture = Some(index);
        crate::streaming::video::metrics::METRICS
            .presented
            .fetch_add(1, Ordering::Relaxed);
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
        if !output.decoder_ready.load(Ordering::Acquire) {
            return Ok(());
        }
        if output_is_current
            && (self.video_textures.is_some() || self.direct_video_output.is_some())
        {
            return Ok(());
        }

        self.detach_direct_video_output();
        let (width, height) = (output.width, output.height);
        // 2b: direct mode -- our own CDRAM buffers as the decode targets
        // (2 display + 1 scratch). RGBA8888 only: the display's second
        // accepted format (0x50000000) is NOT B5G6R5 on this firmware --
        // RGB565 direct produced a black screen with a healthy 60/60 pipeline
        // (decoded=60, presented=60), so RGB565 falls back to the SDL path.
        if crate::shell::direct_present::DIRECT_MODE.load(Ordering::Relaxed)
            && output.color_profile == ColorProfile::Rgba8888
        {
            let (bpp, fmt) = (4u32, 0u32);
            let cap = width.saturating_mul(bpp).saturating_mul(height);
            let pitch = width * bpp;
            let b0 = CdramBlock::allocate("xcloud_fb0", cap)?;
            let b1 = CdramBlock::allocate("xcloud_fb1", cap)?;
            let bs = CdramBlock::allocate("xcloud_fb_scratch", cap)?;
            let t0 = VideoTextureTarget { ptr: b0.ptr as usize, pitch, capacity: cap };
            let t1 = VideoTextureTarget { ptr: b1.ptr as usize, pitch, capacity: cap };
            let ts = VideoTextureTarget { ptr: bs.ptr as usize, pitch, capacity: cap };
            output.set_targets([t0, t1, ts]);
            self.cdram = vec![b0, b1, bs];
            self.direct_bpp = bpp;
            self.direct_fmt = fmt;
            self.video_textures = None;
            self.displayed_video_texture = None;
            self.direct_video_output = Some(output);
            self.video_width = width;
            self.video_height = height;
            self.last_frame_id = 0;
            return Ok(());
        }
        // Match the hardware decoder's output format (ColorProfile). SCE pixel-format
        // naming puts the red channel in the lowest byte/bits (the working RGB565 path
        // is the same convention: SCE "RGBA565" pairs with SDL BGR565), so SCE
        // RGBA8888 = u32 0xAABBGGRR = memory bytes R,G,B,A = SDL ABGR8888. If colors
        // come out swapped on device, flip this to PixelFormatEnum::RGBA8888.
        let format = match output.color_profile {
            ColorProfile::Rgb565 => PixelFormatEnum::BGR565,
            ColorProfile::Rgba8888 => PixelFormatEnum::ABGR8888,
        };
        let create_texture = || {
            self.canvas
                .create_texture_streaming(format, width, height)
                .map_err(anyhow::Error::msg)
                .context("failed to create direct SDL video texture")
        };
        let mut textures: [_; NUM_TEXTURES] = [
            create_texture()?,
            create_texture()?,
            // Decoder-private scratch (stage 3c): retired AUs decode here and
            // are never displayed.
            create_texture()?,
        ];
        let mut targets: [_; NUM_TEXTURES] = [
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

    /// Direct-present experiment: scan the drawn frame out immediately
    /// (tearing) instead of at the next vblank. Returns `false` to fall back
    /// to the normal SDL present.
    pub fn present_direct(&mut self) -> Result<bool> {
        if crate::shell::direct_present::DIRECT_MODE.load(Ordering::Relaxed) {
            let Some(index) = self.displayed_video_texture else {
                return Ok(false);
            };
            if let Some(t) = self
                .direct_video_output
                .as_ref()
                .and_then(|o| o.target(index))
            {
                if t.ptr != 0 {
                    return Ok(crate::shell::direct_present::present_target(
                        t.ptr,
                        t.pitch,
                        self.direct_bpp,
                        self.direct_fmt,
                        WIDTH,
                        HEIGHT,
                    ));
                }
            }
            return Ok(false);
        }
        crate::shell::direct_present::present_immediate(&mut self.canvas, WIDTH, HEIGHT)
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

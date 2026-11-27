//! Direct-present experiment (2026-11-26).
//!
//! The Vita panel latches the framebuffer at the vblank, so an off-vblank
//! `SDL_RenderPresent` is still scanned out at the next vblank. This module
//! writes the drawn frame into the *current* framebuffer and sets it with
//! `SCE_DISPLAY_SETBUF_IMMEDIATE`, which scans it out immediately (tearing).
//!
//! This first version reads the screen back through SDL (slow, ~1 MB/frame)
//! to prove the mechanism. If the win is real, the readback is replaced by
//! writing the decoded frame straight into the framebuffer.
//!
//! Diagnostics: every call increments OK or FALLBACK (with a reason), and a
//! small report is written to `ux0:data/xcloud-rust/direct-present.txt` every
//! 5 s so the FTP pull confirms the direct path is actually firing. Set
//! `STRICT` to true to disable the fallback (a fallback then skips present,
//! leaving the last frame on screen -- a deliberate, visible signal).

use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Master switch for the experiment. Disabled 2026-11-27: with the format fix
/// the direct path fired (direct-present.txt ok=871 fallback=0) but the SDL
/// readback of a 960x544 A8B8G8R8 frame + row copy cost ~79 ms/frame
/// (ui_work_us 78.8, 15 fps) and produced a black/broken image. The readback
/// approach is a dead end; a real direct present needs the decoder to write
/// straight into the framebuffer (no SDL readback).
pub static ENABLED: AtomicBool = AtomicBool::new(false);

/// When true, a fallback does NOT present through SDL: the caller is told the
/// frame was "presented" so the last image stays on screen. Use it to prove the
/// direct path is active (the screen freezes if it is not).
pub static STRICT: AtomicBool = AtomicBool::new(false);

/// 2b: own CDRAM buffers as decode targets (RGBA8888). The decoder writes
/// straight into our buffer; present hands it to SetFrameBuf(IMMEDIATE).
pub static DIRECT_MODE: AtomicBool = AtomicBool::new(true);

static OK: AtomicU64 = AtomicU64::new(0);
static FALLBACK: AtomicU64 = AtomicU64::new(0);
static LAST_REPORT_MS: AtomicU64 = AtomicU64::new(0);

fn report(ok: u64, fallback: u64, reason: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if now.saturating_sub(LAST_REPORT_MS.load(Ordering::Relaxed)) < 5_000 {
        return;
    }
    LAST_REPORT_MS.store(now, Ordering::Relaxed);
    let _ = std::fs::create_dir_all("ux0:data/xcloud-rust");
    let _ = std::fs::write(
        "ux0:data/xcloud-rust/direct-present.txt",
        format!("ok={ok} fallback={fallback} last_reason={reason}\n"),
    );
}

fn note_ok() {
    let ok = OK.fetch_add(1, Ordering::Relaxed) + 1;
    report(ok, FALLBACK.load(Ordering::Relaxed), "-");
}

fn note_fallback(reason: &str) {
    let fb = FALLBACK.fetch_add(1, Ordering::Relaxed) + 1;
    report(OK.load(Ordering::Relaxed), fb, reason);
}

#[cfg(target_os = "vita")]
mod imp {
    use super::{note_fallback, note_ok, ENABLED, STRICT};
    use anyhow::Result;
    use sdl2::pixels::PixelFormatEnum;

    const SCE_DISPLAY_SETBUF_IMMEDIATE: i32 = 0;

    #[repr(C)]
    struct SceDisplayFrameBuf {
        size: usize,
        base: *mut std::ffi::c_void,
        pitch: u32,
        pixelformat: u32,
        width: u32,
        height: u32,
    }

    unsafe extern "C" {
        fn sceDisplayGetFrameBuf(p: *mut SceDisplayFrameBuf, sync: i32) -> i32;
        fn sceDisplaySetFrameBuf(p: *const SceDisplayFrameBuf, sync: i32) -> i32;
    }

    /// 2b: present our own CDRAM buffer. `pitch` is bytes; `bpp` converts it
    /// to the framebuffer's pixel pitch. Formats: 0 = A8B8G8R8 (RGBA8888),
    /// 0x50000000 = B5G6R5 (RGB565).
    pub fn present_target(
        ptr: usize,
        pitch: u32,
        bpp: u32,
        pixelformat: u32,
        width: u32,
        height: u32,
    ) -> bool {
        let fb = SceDisplayFrameBuf {
            size: std::mem::size_of::<SceDisplayFrameBuf>(),
            base: ptr as *mut std::ffi::c_void,
            pitch: pitch / bpp,
            pixelformat,
            width,
            height,
        };
        unsafe { sceDisplaySetFrameBuf(&fb, SCE_DISPLAY_SETBUF_IMMEDIATE) == 0 }
    }

    /// 2a: the display framebuffer as a decode target (single buffer, no
    /// allocation). Returns (base ptr, pitch, capacity) for A8B8G8R8.
    pub fn framebuffer_target() -> Option<(usize, u32, u32)> {
        let mut fb = SceDisplayFrameBuf {
            size: std::mem::size_of::<SceDisplayFrameBuf>(),
            base: std::ptr::null_mut(),
            pitch: 0,
            pixelformat: 0,
            width: 0,
            height: 0,
        };
        let got = unsafe { sceDisplayGetFrameBuf(&mut fb, SCE_DISPLAY_SETBUF_IMMEDIATE) };
        if got != 0 || fb.base.is_null() || fb.pixelformat != 0 {
            return None;
        }
        Some((fb.base as usize, fb.pitch, fb.pitch.saturating_mul(fb.height)))
    }

    /// 2a: present the framebuffer the decoder just wrote (no readback).
    pub fn present_framebuffer() -> bool {
        let mut fb = SceDisplayFrameBuf {
            size: std::mem::size_of::<SceDisplayFrameBuf>(),
            base: std::ptr::null_mut(),
            pitch: 0,
            pixelformat: 0,
            width: 0,
            height: 0,
        };
        let got = unsafe { sceDisplayGetFrameBuf(&mut fb, SCE_DISPLAY_SETBUF_IMMEDIATE) };
        if got != 0 || fb.base.is_null() {
            return false;
        }
        unsafe {
            sceDisplaySetFrameBuf(&fb, SCE_DISPLAY_SETBUF_IMMEDIATE);
        }
        true
    }

    /// Present the already-drawn screen immediately. Returns `true` when the
    /// direct path was used (or STRICT is on), `false` to fall back to SDL.
    pub fn present_immediate(
        canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
        width: u32,
        height: u32,
    ) -> Result<bool> {
        if !ENABLED.load(super::Ordering::Relaxed) {
            return Ok(false);
        }
        let strict = STRICT.load(super::Ordering::Relaxed);

        let mut fb = SceDisplayFrameBuf {
            size: std::mem::size_of::<SceDisplayFrameBuf>(),
            base: std::ptr::null_mut(),
            pitch: 0,
            pixelformat: 0,
            width: 0,
            height: 0,
        };
        let got = unsafe { sceDisplayGetFrameBuf(&mut fb, SCE_DISPLAY_SETBUF_IMMEDIATE) };
        if got != 0 || fb.base.is_null() {
            note_fallback(if got != 0 { "getframebuf" } else { "null-base" });
            return Ok(strict);
        }
        // Accept both Vita framebuffer formats: 0 = A8B8G8R8 (4 B/px),
        // 1 = B5G6R5 (2 B/px). The earlier build only accepted B5G6R5 and
        // silently fell back on this panel (direct-present.txt: format).
        let (sdl_fmt, bpp) = match fb.pixelformat {
            0 => (PixelFormatEnum::ABGR8888, 4usize),
            1 => (PixelFormatEnum::BGR565, 2usize),
            _ => {
                note_fallback("format");
                return Ok(strict);
            }
        };
        if fb.width != width || fb.height != height {
            note_fallback("size");
            return Ok(strict);
        }

        let pixels = match canvas.read_pixels(None, sdl_fmt) {
            Ok(p) => p,
            Err(_) => {
                note_fallback("readpixels");
                return Ok(strict);
            }
        };

        // Row-by-row copy honouring the framebuffer pitch (it may exceed
        // width * bpp).
        let src_pitch = width as usize * bpp;
        let dst_pitch = fb.pitch as usize;
        unsafe {
            for y in 0..height as usize {
                std::ptr::copy_nonoverlapping(
                    pixels.as_ptr().add(y * src_pitch),
                    (fb.base as *mut u8).add(y * dst_pitch),
                    src_pitch,
                );
            }
            sceDisplaySetFrameBuf(&fb, SCE_DISPLAY_SETBUF_IMMEDIATE);
        }
        note_ok();
        Ok(true)
    }
}

#[cfg(not(target_os = "vita"))]
mod imp {
    use anyhow::Result;

    pub fn present_immediate(
        _canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
        _width: u32,
        _height: u32,
    ) -> Result<bool> {
        Ok(false)
    }

    pub fn framebuffer_target() -> Option<(usize, u32, u32)> {
        None
    }

    pub fn present_framebuffer() -> bool {
        false
    }

    pub fn present_target(
        _ptr: usize,
        _pitch: u32,
        _bpp: u32,
        _pixelformat: u32,
        _width: u32,
        _height: u32,
    ) -> bool {
        false
    }
}

pub use imp::present_immediate;
pub use imp::{framebuffer_target, present_framebuffer, present_target};
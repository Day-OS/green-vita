//! PS Vita hardware H.264 decoder (`sceVideodec`/`sceAvcdec`).
use super::memory::{CdramBlock, release_reserved_decoder_cdram};
use super::metrics;
use anyhow::{Result, bail};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use vitasdk_sys::*;

const AVCDEC_NUM_REF_FRAMES: u32 = 3;
// AVCDEC and SDL's Vita GXM renderer both support RGB565 natively. At 960x544 this halves the
// decoder-to-texture traffic from roughly 2 MiB to 1 MiB per frame, which matters at 60 FPS.
const OUTPUT_BYTES_PER_PIXEL: u32 = 2;
const OUTPUT_PIXEL_FORMAT: u32 = SCE_AVCDEC_PIXELFORMAT_RGBA565 as u32;
static DMAC_COPY_FALLBACK_REPORTED: AtomicBool = AtomicBool::new(false);

struct AvcdecLibrary {
    module_loaded: bool,
}

impl AvcdecLibrary {
    fn initialize(width: u32, height: u32) -> Result<Self> {
        let module_loaded = unsafe {
            let loaded_before = sceSysmoduleIsLoaded(SCE_SYSMODULE_AVCDEC);
            let ret = sceSysmoduleLoadModule(SCE_SYSMODULE_AVCDEC);
            if ret >= 0 {
                true
            } else if ret as u32 == SCE_SYSMODULE_ERROR_INVALID_VALUE {
                eprintln!(
                    "sceSysmoduleLoadModule(SCE_SYSMODULE_AVCDEC=0x{SCE_SYSMODULE_AVCDEC:x}) returned {ret:#x}; continuing with SceVideodec imports; is_loaded_before={loaded_before:#x}",
                );
                false
            } else {
                bail!(
                    "sceSysmoduleLoadModule(SCE_SYSMODULE_AVCDEC=0x{SCE_SYSMODULE_AVCDEC:x}) failed: {ret:#x}; is_loaded_before={loaded_before:#x}",
                );
            }
        };

        let init_info = SceVideodecQueryInitInfoHwAvcdec {
            size: size_of::<SceVideodecQueryInitInfoHwAvcdec>() as u32,
            horizontal: width,
            vertical: height,
            numOfRefFrames: AVCDEC_NUM_REF_FRAMES,
            numOfStreams: 1,
        };
        let ret = unsafe { sceVideodecInitLibrary(SCE_VIDEODEC_TYPE_HW_AVCDEC, &init_info) };
        if ret < 0 {
            if module_loaded {
                unsafe {
                    sceSysmoduleUnloadModule(SCE_SYSMODULE_AVCDEC);
                }
            }
            bail!("sceVideodecInitLibrary failed: {ret:#x}");
        }

        Ok(Self { module_loaded })
    }
}

impl Drop for AvcdecLibrary {
    fn drop(&mut self) {
        unsafe {
            sceVideodecTermLibrary(SCE_VIDEODEC_TYPE_HW_AVCDEC);
            if self.module_loaded {
                sceSysmoduleUnloadModule(SCE_SYSMODULE_AVCDEC);
            }
        }
    }
}

struct AvcdecDecoder(SceAvcdecCtrl);

impl Drop for AvcdecDecoder {
    fn drop(&mut self) {
        unsafe {
            sceAvcdecDeleteDecoder(&mut self.0);
        }
    }
}

fn frame_output_size(height: u32, pitch: u32) -> Result<u32> {
    let row_bytes = pitch
        .checked_mul(OUTPUT_BYTES_PER_PIXEL)
        .ok_or_else(|| anyhow::anyhow!("video frame pitch overflow: {pitch}"))?;
    row_bytes
        .checked_mul(height)
        .ok_or_else(|| anyhow::anyhow!("video output size overflow: {row_bytes} * {height}"))
}

pub struct HwVideoDecoder {
    decoder: AvcdecDecoder,
    output: CdramBlock,
    _frame_memory: CdramBlock,
    _library: AvcdecLibrary,
    output_len: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
}

impl HwVideoDecoder {
    pub fn new(
        decode_width: u32,
        decode_height: u32,
        output_width: u32,
        output_height: u32,
    ) -> Result<Self> {
        unsafe {
            let library = AvcdecLibrary::initialize(decode_width, decode_height)?;

            let query = SceAvcdecQueryDecoderInfo {
                horizontal: decode_width,
                vertical: decode_height,
                numOfRefFrames: AVCDEC_NUM_REF_FRAMES,
            };
            let mut decoder_info = SceAvcdecDecoderInfo { frameMemSize: 0 };
            let ret = sceAvcdecQueryDecoderMemSize(
                SCE_VIDEODEC_TYPE_HW_AVCDEC,
                &query,
                &mut decoder_info,
            );
            if ret < 0 {
                bail!("sceAvcdecQueryDecoderMemSize failed: {ret:#x}");
            }
            release_reserved_decoder_cdram();
            let frame_memory =
                CdramBlock::allocate("xcloud_hw_video_frame", decoder_info.frameMemSize)?;
            metrics::record_decoder_frame_memory(decoder_info.frameMemSize, frame_memory.capacity);

            let mut decoder_control = SceAvcdecCtrl {
                handle: 0,
                frameBuf: SceAvcdecBuf {
                    pBuf: frame_memory.ptr.cast(),
                    size: decoder_info.frameMemSize,
                },
            };
            let ret =
                sceAvcdecCreateDecoder(SCE_VIDEODEC_TYPE_HW_AVCDEC, &mut decoder_control, &query);
            if ret < 0 {
                bail!("sceAvcdecCreateDecoder failed: {ret:#x}");
            }
            let decoder = AvcdecDecoder(decoder_control);

            let output_len = frame_output_size(output_height, output_width)?;
            let output = CdramBlock::allocate("xcloud_hw_video_out", output_len)?;
            metrics::record_decoder_output_memory(output_len, output.capacity);

            Ok(Self {
                decoder,
                output,
                _frame_memory: frame_memory,
                _library: library,
                output_len,
                width: output_width,
                height: output_height,
                pitch: 0,
            })
        }
    }

    /// Borrows the hardware's own output buffer - copied exactly once, straight into the texture.
    fn frame_bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.output.ptr, self.output_len as usize) }
    }

    /// Moves the non-cached AVCDEC output into an owned frame through the Vita DMA engine. Reading
    /// that memory with a CPU memcpy is considerably slower than decoding the frame itself.
    pub fn copy_frame_bytes(&self) -> Vec<u8> {
        let len = self.output_len as usize;
        let mut frame: Vec<u8> = Vec::with_capacity(len);
        let ret = unsafe {
            sceDmacMemcpy(
                frame.as_mut_ptr().cast(),
                self.output.ptr.cast(),
                self.output_len,
            )
        };
        if ret >= 0 {
            unsafe {
                frame.set_len(len);
            }
            return frame;
        }

        if !DMAC_COPY_FALLBACK_REPORTED.swap(true, Ordering::Relaxed) {
            eprintln!("sceDmacMemcpy decoder copy failed ({ret:#x}); using CPU fallback");
        }
        frame.extend_from_slice(self.frame_bytes());
        frame
    }

    /// Decodes one Access Unit. Returns `false` if the hardware buffered it without producing a picture yet.
    pub fn decode(&mut self, access_unit: &[u8]) -> Result<bool> {
        unsafe {
            let au = SceAvcdecAu {
                pts: SceVideodecTimeStamp {
                    upper: 0xFFFFFFFF,
                    lower: 0xFFFFFFFF,
                },
                dts: SceVideodecTimeStamp {
                    upper: 0xFFFFFFFF,
                    lower: 0xFFFFFFFF,
                },
                es: SceAvcdecBuf {
                    pBuf: access_unit.as_ptr() as *mut c_void,
                    size: access_unit.len() as u32,
                },
            };

            let mut picture = SceAvcdecPicture {
                size: size_of::<SceAvcdecPicture>() as u32,
                frame: SceAvcdecFrame {
                    pixelType: OUTPUT_PIXEL_FORMAT,
                    framePitch: self.width,
                    frameWidth: self.width,
                    frameHeight: self.height,
                    horizontalSize: self.width,
                    verticalSize: self.height,
                    frameCropLeftOffset: 0,
                    frameCropRightOffset: 0,
                    frameCropTopOffset: 0,
                    frameCropBottomOffset: 0,
                    opt: SceAvcdecFrameOption {
                        rgba: SceAvcdecFrameOptionRGBA {
                            alpha: 0xff,
                            cscCoefficient: 0,
                            reserved: [0; 14],
                        },
                    },
                    pPicture: [self.output.ptr.cast(), std::ptr::null_mut()],
                },
                info: std::mem::zeroed(),
            };
            let mut picture_ptr: *mut SceAvcdecPicture = &mut picture;
            let mut array_picture = SceAvcdecArrayPicture {
                numOfOutput: 0,
                numOfElm: 1,
                pPicture: &mut picture_ptr,
            };

            let ret = sceAvcdecDecode(&self.decoder.0, &au, &mut array_picture);
            if ret < 0 {
                bail!("sceAvcdecDecode failed: {ret:#x}");
            }
            if array_picture.numOfOutput == 0 {
                return Ok(false);
            }

            // `framePitch` is in pixels, not bytes.
            let output_len = frame_output_size(self.height, picture.frame.framePitch)?;
            if output_len > self.output.capacity {
                bail!(
                    "sceAvcdecDecode produced pitch requiring {output_len} bytes, but output buffer has {} bytes",
                    self.output.capacity
                );
            }
            self.pitch = picture.frame.framePitch * OUTPUT_BYTES_PER_PIXEL;
            self.output_len = output_len;
            Ok(true)
        }
    }
}

// SAFETY: the CDRAM blocks and decoder handle have no thread affinity in the underlying SCE API -
// this is only ever moved once (into `VideoDecodeWorker`'s thread), never accessed concurrently.
unsafe impl Send for HwVideoDecoder {}

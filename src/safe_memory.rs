use anyhow::{Result, bail};
use std::ffi::c_void;
use vitasdk_sys::{
    SceAppUtilBootParam, SceAppUtilInitParam, sceAppUtilInit, sceAppUtilLoadSafeMemory,
    sceAppUtilSaveSafeMemory, sceAppUtilShutdown,
};

/// Keeps AppUtil initialized for every Safe Memory access made by the application.
pub struct AppUtil;

impl AppUtil {
    pub fn initialize() -> Result<Self> {
        let mut init: SceAppUtilInitParam = unsafe { std::mem::zeroed() };
        let mut boot: SceAppUtilBootParam = unsafe { std::mem::zeroed() };
        let result = unsafe { sceAppUtilInit(&mut init, &mut boot) };
        if result < 0 {
            bail!("sceAppUtilInit failed: {result:#x}");
        }
        Ok(Self)
    }
}

impl Drop for AppUtil {
    fn drop(&mut self) {
        unsafe {
            sceAppUtilShutdown();
        }
    }
}

pub fn load<const N: usize>(offset: i64) -> Result<[u8; N]> {
    let mut data = [0u8; N];
    let result =
        unsafe { sceAppUtilLoadSafeMemory(data.as_mut_ptr().cast::<c_void>(), N as u32, offset) };
    if result < 0 {
        bail!("sceAppUtilLoadSafeMemory failed: {result:#x}");
    }
    Ok(data)
}

pub fn save(offset: i64, data: &[u8]) -> Result<()> {
    let result = unsafe {
        sceAppUtilSaveSafeMemory(
            data.as_ptr().cast_mut().cast::<c_void>(),
            data.len() as u32,
            offset,
        )
    };
    if result < 0 {
        bail!("sceAppUtilSaveSafeMemory failed: {result:#x}");
    }
    Ok(())
}

//! Streaming power profile: raises the CPU and memory-bus clocks and keeps
//! auto-suspend/screen-off at bay while the app runs. The previous values are
//! restored on drop.
//!
//! Stage 5a (2026-11-27): CPU + memory bus only. The GPU/xbar are left at the
//! OS default (the GPU is not the bottleneck: one blit + a few HUD quads), and
//! Wi-Fi is owned by TurboWifi, so `SetUsingWireless` is not called. An
//! external overclock plugin may already set these; the SDK calls are
//! idempotent.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vitasdk_sys::{
    sceKernelPowerTick, scePowerGetArmClockFrequency, scePowerGetBusClockFrequency,
    scePowerGetGpuClockFrequency, scePowerGetGpuXbarClockFrequency, scePowerGetUsingWireless,
    scePowerSetArmClockFrequency, scePowerSetBusClockFrequency, scePowerSetGpuClockFrequency,
    scePowerSetGpuXbarClockFrequency, scePowerSetUsingWireless,
    SCE_KERNEL_POWER_TICK_DISABLE_AUTO_SUSPEND, SCE_KERNEL_POWER_TICK_DISABLE_OLED_OFF,
};

const ARM_CLOCK_MHZ: i32 = 444;
const BUS_CLOCK_MHZ: i32 = 222;
const POWER_TICK_INTERVAL: Duration = Duration::from_secs(10);
const POWER_TICK_MASK: u32 =
    (SCE_KERNEL_POWER_TICK_DISABLE_AUTO_SUSPEND | SCE_KERNEL_POWER_TICK_DISABLE_OLED_OFF) as u32;

/// Stage 5a: CPU + memory bus overclock, engaged while streaming.
pub(crate) const ENABLED: bool = true;

struct SavedClocks {
    arm: i32,
    bus: i32,
    gpu: i32,
    gpu_xbar: i32,
    wireless: i32,
}

/// Held for the app's lifetime; restores the previous power state on drop.
pub struct StreamingPowerGuard {
    stop: Arc<AtomicBool>,
    ticker: Option<thread::JoinHandle<()>>,
    saved: SavedClocks,
}

pub fn engage() -> StreamingPowerGuard {
    if !ENABLED {
        return StreamingPowerGuard {
            stop: Arc::new(AtomicBool::new(true)),
            ticker: None,
            saved: SavedClocks {
                arm: 0,
                bus: 0,
                gpu: 0,
                gpu_xbar: 0,
                wireless: 0,
            },
        };
    }
    let saved = SavedClocks {
        arm: unsafe { scePowerGetArmClockFrequency() },
        bus: unsafe { scePowerGetBusClockFrequency() },
        gpu: unsafe { scePowerGetGpuClockFrequency() },
        gpu_xbar: unsafe { scePowerGetGpuXbarClockFrequency() },
        wireless: unsafe { scePowerGetUsingWireless() },
    };

    unsafe {
        let _ = scePowerSetArmClockFrequency(ARM_CLOCK_MHZ);
        let _ = scePowerSetBusClockFrequency(BUS_CLOCK_MHZ);
        // Stage 5a: GPU/xbar left at the OS default; Wi-Fi owned by TurboWifi.
        // Read back and record the real clocks (the CSV column was never
        // written before, so 333 vs 444 could not be verified).
        let arm = unsafe { scePowerGetArmClockFrequency() };
        let bus = unsafe { scePowerGetBusClockFrequency() };
        crate::streaming::video::metrics::METRICS
            .clk_arm_mhz
            .store(arm.max(0) as u64, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .clk_bus_mhz
            .store(bus.max(0) as u64, Ordering::Relaxed);
    }
    eprintln!(
        "streaming power profile engaged (was arm:{} bus:{} gpu:{} xbar:{} wireless:{})",
        saved.arm, saved.bus, saved.gpu, saved.gpu_xbar, saved.wireless
    );

    let stop = Arc::new(AtomicBool::new(false));
    let ticker_stop = Arc::clone(&stop);
    let ticker = thread::Builder::new()
        .name("power-tick".into())
        .spawn(move || {
            while !ticker_stop.load(Ordering::Relaxed) {
                unsafe {
                    let _ = sceKernelPowerTick(POWER_TICK_MASK);
                }
                thread::sleep(POWER_TICK_INTERVAL);
            }
        })
        .ok();

    StreamingPowerGuard {
        stop,
        ticker,
        saved,
    }
}

impl Drop for StreamingPowerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        unsafe {
            let _ = scePowerSetArmClockFrequency(self.saved.arm);
            let _ = scePowerSetBusClockFrequency(self.saved.bus);
            let _ = scePowerSetGpuClockFrequency(self.saved.gpu);
            let _ = scePowerSetGpuXbarClockFrequency(self.saved.gpu_xbar);
            let _ = scePowerSetUsingWireless(self.saved.wireless);
        }
        eprintln!("streaming power profile restored");
    }
}

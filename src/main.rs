use vita_newlib_shims as _;

mod app;
mod i18n;
mod input;
mod safe_memory;
mod settings;
mod shell;
mod streaming;
mod xbox_api;

use app::{App, AppCommand, AppState, InputCommand, NavigationCommand};
use settings::Locale;
use streaming::control::input::{GamepadFrame, PointerEvent};
use streaming::rtc::session::{HW_DECODE_HEIGHT, HW_DECODE_WIDTH, STREAM_HEIGHT, STREAM_WIDTH};
use xbox_api::api::{
    ApiClient, ApiClientConfig, Console, ConsolesResponse, StreamKind, WaitTimeResponse,
};
use xbox_api::auth::{DeviceCodeAuth, DeviceCodePoll, MsalAuth, StreamingCredentials, XboxProfile};
use xbox_api::stream::{Stream, StreamState};

#[used]
#[unsafe(export_name = "sceUserMainThreadStackSize")]
pub static SCE_USER_MAIN_THREAD_STACK_SIZE: u32 = 4 * 1024 * 1024;

#[used]
#[unsafe(export_name = "sceLibcHeapSize")]
pub static SCE_LIBC_HEAP_SIZE: u32 = 40 * 1024 * 1024;

#[used]
#[unsafe(export_name = "_newlib_heap_size_user")]
pub static NEWLIB_HEAP_SIZE_USER: u32 = 192 * 1024 * 1024;

mod fs_utils {
    use anyhow::{Context, Result};

    /// Removes `path` before writing - `std::fs::write` alone doesn't reliably truncate an
    /// existing file on the Vita's newlib filesystem.
    pub fn write_file_truncating(path: &str, data: impl AsRef<[u8]>) -> Result<()> {
        let _ = std::fs::remove_file(path);
        std::fs::write(path, data).with_context(|| format!("failed to write {path}"))
    }
}

fn main() -> anyhow::Result<()> {
    let _app_util = safe_memory::AppUtil::initialize()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let app = App::new()?;
        shell::run(app).await
    })
}

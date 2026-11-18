use eframe::egui;
use libloading::os::windows::{
    Library, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::OnceLock;

const FEED: &CStr = c"https://raw.githubusercontent.com/geek0609/balance/updates/windows-x64.xml";
const REGISTRY_PATH: &CStr = c"Software\\Balance\\Updates";
const RELEASE_VERSION: &str = match option_env!("BALANCE_UPDATE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

static APP_CONTEXT: OnceLock<egui::Context> = OnceLock::new();

#[derive(Default)]
pub(crate) struct Updates {
    sparkle: Option<Result<Sparkle, String>>,
}

impl Updates {
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        let sparkle = self
            .sparkle
            .get_or_insert_with(|| Sparkle::start(ui.ctx()).map_err(|error| error.to_string()));
        ui.menu_button("Updates", |ui| {
            ui.label(format!("Balance {RELEASE_VERSION}"));
            match sparkle {
                Ok(api) => {
                    let mut automatic = unsafe { (api.get_automatic)() != 0 };
                    if ui
                        .checkbox(&mut automatic, "Automatically check for updates")
                        .changed()
                    {
                        unsafe { (api.set_automatic)(c_int::from(automatic)) };
                    }
                    if ui.button("Check for updates").clicked() {
                        unsafe { (api.check)() };
                    }
                }
                Err(error) => {
                    ui.label("Updater unavailable");
                    ui.small(error.as_str());
                }
            }
        });
    }
}

type Action = unsafe extern "C" fn();
type SetAutomatic = unsafe extern "C" fn(c_int);
type GetAutomatic = unsafe extern "C" fn() -> c_int;

struct Sparkle {
    _library: Library,
    cleanup: Action,
    check: Action,
    set_automatic: SetAutomatic,
    get_automatic: GetAutomatic,
}

impl Sparkle {
    fn start(ctx: &egui::Context) -> Result<Self, Box<dyn std::error::Error>> {
        let key = option_env!("BALANCE_UPDATE_PUBLIC_KEY").ok_or("Missing update public key")?;
        let build = option_env!("BALANCE_UPDATE_BUILD").ok_or("Missing update build number")?;
        let key = CString::new(key)?;
        let version = wide(RELEASE_VERSION);
        let build = wide(build);
        let publisher = wide("Balance");
        let path = std::env::current_exe()?.with_file_name("WinSparkle.dll");

        // Load the bundled DLL by absolute path. Its dependencies may come from
        // the install directory or System32, independent of the working directory.
        let library = unsafe {
            Library::load_with_flags(
                path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )?
        };
        unsafe {
            // These signatures match the pinned WinSparkle C header. Resolve
            // all symbols before initialization so cleanup is always available.
            let init: Action = symbol(&library, c"win_sparkle_init")?;
            let cleanup: Action = symbol(&library, c"win_sparkle_cleanup")?;
            let check: Action = symbol(&library, c"win_sparkle_check_update_with_ui")?;
            let get_automatic: GetAutomatic =
                symbol(&library, c"win_sparkle_get_automatic_check_for_updates")?;
            let set_automatic: SetAutomatic =
                symbol(&library, c"win_sparkle_set_automatic_check_for_updates")?;
            let set_feed: unsafe extern "C" fn(*const c_char) =
                symbol(&library, c"win_sparkle_set_appcast_url")?;
            let set_key: unsafe extern "C" fn(*const c_char) -> c_int =
                symbol(&library, c"win_sparkle_set_eddsa_public_key")?;
            let set_registry: unsafe extern "C" fn(*const c_char) =
                symbol(&library, c"win_sparkle_set_registry_path")?;
            let set_details: unsafe extern "C" fn(*const u16, *const u16, *const u16) =
                symbol(&library, c"win_sparkle_set_app_details")?;
            let set_build: unsafe extern "C" fn(*const u16) =
                symbol(&library, c"win_sparkle_set_app_build_version")?;
            let set_can_shutdown: unsafe extern "C" fn(extern "C" fn() -> c_int) =
                symbol(&library, c"win_sparkle_set_can_shutdown_callback")?;
            let set_shutdown: unsafe extern "C" fn(extern "C" fn()) =
                symbol(&library, c"win_sparkle_set_shutdown_request_callback")?;

            if set_key(key.as_ptr()) != 1 {
                return Err("Invalid update public key".into());
            }
            set_details(publisher.as_ptr(), publisher.as_ptr(), version.as_ptr());
            set_build(build.as_ptr());
            set_feed(FEED.as_ptr());
            set_registry(REGISTRY_PATH.as_ptr());
            APP_CONTEXT
                .set(ctx.clone())
                .map_err(|_| "Updater already initialized")?;
            set_can_shutdown(can_shutdown);
            set_shutdown(request_shutdown);
            init();

            Ok(Self {
                _library: library,
                cleanup,
                check,
                get_automatic,
                set_automatic,
            })
        }
    }
}

impl Drop for Sparkle {
    fn drop(&mut self) {
        unsafe { (self.cleanup)() };
    }
}

unsafe fn symbol<T: Copy>(library: &Library, name: &CStr) -> Result<T, libloading::Error> {
    Ok(*library.get(name.to_bytes_with_nul())?)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

extern "C" fn can_shutdown() -> c_int {
    // Install is user-confirmed. The UI thread completes every in-flight
    // database transaction before processing the queued window close.
    1
}

extern "C" fn request_shutdown() {
    if let Some(ctx) = APP_CONTEXT.get() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

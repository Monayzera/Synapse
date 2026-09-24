use crate::state::AppState;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
#[cfg(target_os = "macos")]
use crate::state::SharedState;
#[cfg(target_os = "macos")]
use tauri::{AppHandle, Manager};

static MIC_DENIED: AtomicBool = AtomicBool::new(false);
static ACCESSIBILITY: AtomicU8 = AtomicU8::new(0);

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessibilityIssue {
    Missing,
    Stale,
}

#[cfg(target_os = "macos")]
impl AccessibilityIssue {
    fn id(self) -> u8 {
        match self {
            AccessibilityIssue::Missing => 1,
            AccessibilityIssue::Stale => 2,
        }
    }

    fn code(self) -> &'static str {
        match self {
            AccessibilityIssue::Missing => "accessibility_needed",
            AccessibilityIssue::Stale => "accessibility_stale",
        }
    }

    fn message(self) -> &'static str {
        match self {
            AccessibilityIssue::Missing => "Allow Synapse in System Settings > Privacy & Security > Accessibility. The global shortcut starts working as soon as you enable it.",
            AccessibilityIssue::Stale => "macOS is blocking the global shortcut even though Accessibility looks enabled (stale permission from a previous build). In System Settings > Privacy & Security > Accessibility, remove Synapse with the minus button, add it again and turn it on.",
        }
    }
}

#[cfg(target_os = "macos")]
const MIC_DENIED_MESSAGE: &str =
    "macOS is blocking the microphone for Synapse. Turn it on in System Settings > Privacy & Security > Microphone.";
#[cfg(target_os = "macos")]
const MIC_PROMPT_MESSAGE: &str =
    "Allow microphone access for Synapse in the macOS prompt, then try again.";
#[cfg(windows)]
const MIC_DENIED_MESSAGE: &str =
    "Windows is blocking the microphone for desktop apps. Turn it on in Settings > Privacy & security > Microphone.";
#[cfg(not(any(windows, target_os = "macos")))]
const MIC_DENIED_MESSAGE: &str = "The operating system is blocking microphone access.";

#[cfg(target_os = "macos")]
const MIC_SETTINGS_URL: Option<&str> =
    Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone");
#[cfg(target_os = "macos")]
const ACCESSIBILITY_SETTINGS_URL: Option<&str> =
    Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
#[cfg(windows)]
const MIC_SETTINGS_URL: Option<&str> = Some("ms-settings:privacy-microphone");
#[cfg(windows)]
const ACCESSIBILITY_SETTINGS_URL: Option<&str> = None;
#[cfg(not(any(windows, target_os = "macos")))]
const MIC_SETTINGS_URL: Option<&str> = None;
#[cfg(not(any(windows, target_os = "macos")))]
const ACCESSIBILITY_SETTINGS_URL: Option<&str> = None;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Allowed,
    Denied,
    #[cfg(target_os = "macos")]
    Undetermined,
}

pub fn mic_denied() -> bool {
    MIC_DENIED.load(Ordering::Acquire)
}

pub fn accessibility_needed() -> bool {
    ACCESSIBILITY.load(Ordering::Acquire) != 0
}

pub fn init(state: &AppState) {
    let access = update_mic(state);
    tracing::info!("microphone access at startup: {access:?}");
    #[cfg(target_os = "macos")]
    if access == Access::Undetermined {
        mac::request(&state.app);
    }
}

pub fn refresh_mic(state: &AppState) {
    update_mic(state);
}

pub fn mic_block(state: &AppState) -> Option<(&'static str, &'static str)> {
    match update_mic(state) {
        Access::Allowed => None,
        Access::Denied => {
            #[cfg(windows)]
            state.audio.refresh();
            Some(("mic_denied", MIC_DENIED_MESSAGE))
        }
        #[cfg(target_os = "macos")]
        Access::Undetermined => {
            mac::request(&state.app);
            Some(("mic_prompt", MIC_PROMPT_MESSAGE))
        }
    }
}

#[cfg(target_os = "macos")]
pub fn mic_prompt_pending() -> bool {
    mac::pending()
}

#[cfg(target_os = "macos")]
pub fn set_accessibility_issue(app: &AppHandle, issue: Option<AccessibilityIssue>) {
    let next = issue.map(AccessibilityIssue::id).unwrap_or(0);
    if ACCESSIBILITY.swap(next, Ordering::AcqRel) == next {
        return;
    }
    match issue {
        Some(issue) => crate::pipeline::emit_error(app, "hotkey", issue.code(), issue.message()),
        None => tracing::info!("Accessibility permission is effective"),
    }
    if let Some(state) = app.try_state::<SharedState>() {
        state.emit_status();
    }
}

pub fn open_privacy_settings(kind: &str) -> Result<(), String> {
    let url = match kind {
        "microphone" => MIC_SETTINGS_URL,
        "accessibility" => ACCESSIBILITY_SETTINGS_URL,
        other => return Err(format!("unknown privacy settings kind: {other}")),
    }
    .ok_or_else(|| "unsupported".to_string())?;
    tracing::info!("opening {kind} privacy settings");
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|err| {
        tracing::warn!("{kind} privacy settings could not be opened: {err}");
        err.to_string()
    })
}

fn update_mic(state: &AppState) -> Access {
    let access = current_access(state);
    let denied = access == Access::Denied;
    if MIC_DENIED.swap(denied, Ordering::AcqRel) != denied {
        if denied {
            tracing::warn!("microphone blocked by the operating system ({})", deny_reason());
        } else {
            tracing::info!("microphone access allowed again");
            if cfg!(target_os = "macos") || !state.audio.is_available() {
                state.audio.refresh();
            }
        }
        state.emit_status();
    }
    access
}

fn current_access(state: &AppState) -> Access {
    #[cfg(target_os = "macos")]
    {
        let _ = state;
        match mac::status() {
            Some(mac::NOT_DETERMINED) => Access::Undetermined,
            Some(mac::RESTRICTED) | Some(mac::DENIED) => Access::Denied,
            _ => Access::Allowed,
        }
    }
    #[cfg(windows)]
    {
        if state.audio.is_settled() && !state.audio.is_available() && win::deny_source().is_some() {
            Access::Denied
        } else {
            Access::Allowed
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = state;
        Access::Allowed
    }
}

fn deny_reason() -> &'static str {
    #[cfg(windows)]
    {
        win::deny_source().unwrap_or("privacy settings")
    }
    #[cfg(target_os = "macos")]
    {
        "System Settings > Privacy & Security > Microphone"
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        "privacy settings"
    }
}

#[cfg(target_os = "macos")]
fn mic_request_finished(app: &AppHandle, granted: bool) {
    tracing::info!(
        "microphone permission {}",
        if granted { "granted" } else { "denied" }
    );
    MIC_DENIED.store(!granted, Ordering::Release);
    if let Some(state) = app.try_state::<SharedState>() {
        if granted {
            state.audio.refresh();
        }
        state.emit_status();
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use std::ffi::c_void;
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tauri::AppHandle;

    pub const NOT_DETERMINED: isize = 0;
    pub const RESTRICTED: isize = 1;
    pub const DENIED: isize = 2;

    static PENDING: AtomicBool = AtomicBool::new(false);

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        #[allow(non_upper_case_globals)]
        static AVMediaTypeAudio: *const c_void;
    }

    fn capture_device() -> Option<(&'static AnyClass, &'static AnyObject)> {
        let class = AnyClass::get(c"AVCaptureDevice")?;
        let media: &'static AnyObject = unsafe { AVMediaTypeAudio.cast::<AnyObject>().as_ref() }?;
        Some((class, media))
    }

    pub fn status() -> Option<isize> {
        let (class, media) = capture_device()?;
        let status: isize =
            autoreleasepool(|_| unsafe { msg_send![class, authorizationStatusForMediaType: media] });
        Some(status)
    }

    pub fn pending() -> bool {
        PENDING.load(Ordering::Acquire)
    }

    pub fn request(app: &AppHandle) {
        if PENDING.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some((class, media)) = capture_device() else {
            PENDING.store(false, Ordering::Release);
            tracing::warn!("AVCaptureDevice unavailable; microphone permission not requested");
            return;
        };
        let handle = app.clone();
        let block: RcBlock<dyn Fn(Bool)> = RcBlock::new(move |granted: Bool| {
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                PENDING.store(false, Ordering::Release);
                super::mic_request_finished(&handle, granted.as_bool());
            }));
        });
        tracing::info!("requesting microphone permission");
        autoreleasepool(|_| unsafe {
            let _: () = msg_send![
                class,
                requestAccessForMediaType: media,
                completionHandler: &*block
            ];
        });
    }
}

#[cfg(windows)]
mod win {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };

    const CONSENT: &str =
        r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
    const CONSENT_DESKTOP: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone\NonPackaged";
    const POLICY: &str = r"Software\Policies\Microsoft\Windows\AppPrivacy";
    const POLICY_FORCE_DENY: u32 = 2;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn read_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
        let subkey = wide(subkey);
        let value = wide(value);
        let mut buffer = [0u16; 64];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        let code = unsafe {
            RegGetValueW(
                root,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast::<core::ffi::c_void>()),
                Some(&mut size as *mut u32),
            )
        };
        if code != ERROR_SUCCESS {
            return None;
        }
        let units = (size as usize / 2).min(buffer.len());
        let end = buffer[..units]
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units);
        Some(String::from_utf16_lossy(&buffer[..end]))
    }

    fn read_dword(root: HKEY, subkey: &str, value: &str) -> Option<u32> {
        let subkey = wide(subkey);
        let value = wide(value);
        let mut data: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let code = unsafe {
            RegGetValueW(
                root,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_DWORD,
                None,
                Some((&mut data as *mut u32).cast::<core::ffi::c_void>()),
                Some(&mut size as *mut u32),
            )
        };
        if code != ERROR_SUCCESS {
            return None;
        }
        Some(data)
    }

    fn denies(root: HKEY, subkey: &str) -> bool {
        read_string(root, subkey, "Value")
            .is_some_and(|text| text.trim().eq_ignore_ascii_case("Deny"))
    }

    pub fn deny_source() -> Option<&'static str> {
        if read_dword(HKEY_LOCAL_MACHINE, POLICY, "LetAppsAccessMicrophone") == Some(POLICY_FORCE_DENY) {
            return Some("group policy: Let Windows apps access the microphone = Force Deny");
        }
        if denies(HKEY_LOCAL_MACHINE, CONSENT) {
            return Some("Microphone access is off for this device");
        }
        if denies(HKEY_CURRENT_USER, CONSENT) {
            return Some("Let apps access your microphone is off");
        }
        if denies(HKEY_CURRENT_USER, CONSENT_DESKTOP) {
            return Some("Let desktop apps access your microphone is off");
        }
        None
    }
}

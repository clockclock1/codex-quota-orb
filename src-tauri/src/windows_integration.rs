use std::{env, thread, time::Duration};
use windows::{
    core::{Interface, HSTRING},
    Win32::{
        Storage::EnhancedStorage::PKEY_Title,
        System::{
            Com::StructuredStorage::PROPVARIANT,
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_APARTMENTTHREADED,
            },
        },
        UI::Shell::{
            Common::{IObjectArray, IObjectCollection},
            DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW,
            PropertiesSystem::IPropertyStore,
            SetCurrentProcessExplicitAppUserModelID, ShellLink,
        },
    },
};

pub const APP_USER_MODEL_ID: &str = "com.codex.quota.desktop";

pub fn initialize_taskbar() {
    let app_id = HSTRING::from(APP_USER_MODEL_ID);
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&app_id);
    }
}

pub fn refresh_taskbar() {
    thread::spawn(|| {
        // Explorer may not have created the taskbar entry yet on a first launch or
        // immediately after an upgrade. Retry after the app and tray are ready.
        for delay in [0, 200, 500, 1_000, 2_000, 4_000] {
            if delay != 0 {
                thread::sleep(Duration::from_millis(delay));
            }
            if register_jump_list().is_ok() {
                break;
            }
        }
    });
}

unsafe fn create_task(
    executable: &HSTRING,
    arguments: &str,
    title: &str,
) -> windows::core::Result<IShellLinkW> {
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
    let arguments = HSTRING::from(arguments);
    let title = HSTRING::from(title);
    unsafe {
        link.SetPath(executable)?;
        link.SetArguments(&arguments)?;
        link.SetIconLocation(executable, 0)?;
        link.SetDescription(&title)?;
    }
    let store: IPropertyStore = link.cast()?;
    let value = PROPVARIANT::from(title.to_string_lossy().as_str());
    unsafe {
        store.SetValue(&PKEY_Title, &value)?;
        store.Commit()?;
    }
    Ok(link)
}

fn register_jump_list() -> windows::core::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let result = (|| {
            let executable = HSTRING::from(env::current_exe()?.to_string_lossy().as_ref());
            let app_id = HSTRING::from(APP_USER_MODEL_ID);
            let destination: ICustomDestinationList =
                CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
            destination.SetAppID(&app_id)?;
            let mut minimum_slots = 0;
            let _: IObjectArray = destination.BeginList(&mut minimum_slots)?;
            let tasks: IObjectCollection =
                CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;

            for (arguments, title) in [
                ("--show-main", "显示主窗口"),
                ("--toggle-floating", "显示 / 隐藏悬浮窗"),
                ("--toggle-pin", "固定 / 取消固定悬浮窗"),
                ("--quit", "完全退出"),
            ] {
                let link = create_task(&executable, arguments, title)?;
                tasks.AddObject(&link)?;
            }

            let task_array: IObjectArray = tasks.cast()?;
            destination.AddUserTasks(&task_array)?;
            destination.CommitList()
        })();
        CoUninitialize();
        result
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "modifies the current user's Windows jump list"]
    fn windows_accepts_jump_list_registration() {
        std::thread::spawn(super::register_jump_list)
            .join()
            .expect("jump-list registration thread panicked")
            .expect("Windows rejected the jump-list tasks");
    }
}

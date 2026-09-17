//! Ярлык `.lnk` — для меню «Пуск» (PLAN.md §6/P7).
//!
//! Через COM (`IShellLinkW` + `IPersistFile`): в `windows-sys` интерфейсов COM нет, поэтому
//! здесь — крейт `windows`.

use std::path::Path;

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, IPersistFile,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, Interface};

pub fn create(link: &Path, target: &Path, description: &str) -> windows::core::Result<()> {
    unsafe {
        // Поток может уже быть инициализирован иначе — это не мешает созданию ярлыка.
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| {
            let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shell_link.SetPath(&HSTRING::from(target.as_os_str()))?;
            if let Some(dir) = target.parent() {
                shell_link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            shell_link.SetDescription(&HSTRING::from(description))?;
            shell_link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0)?;
            let file: IPersistFile = shell_link.cast()?;
            file.Save(&HSTRING::from(link.as_os_str()), true)
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shortcut_is_written() {
        let dir = std::env::temp_dir().join(format!("mh-shortcut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("MH Monitoring.lnk");
        let target = std::env::current_exe().unwrap();
        create(&link, &target, "проверка").unwrap();
        assert!(std::fs::metadata(&link).unwrap().len() > 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

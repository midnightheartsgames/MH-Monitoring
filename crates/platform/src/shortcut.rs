//! Ярлык `.lnk` — для меню «Пуск» (PLAN.md §6/P7) и для запуска игры (§6/P10).
//!
//! Через COM (`IShellLinkW` + `IPersistFile`): в `windows-sys` интерфейсов COM нет, поэтому
//! здесь — крейт `windows`.

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IPersistFile,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink,
};
use windows::core::{HSTRING, Interface};

/// Что запускает ярлык.
pub struct Shortcut<'a> {
    pub target: &'a Path,
    pub arguments: &'a str,
    pub description: &'a str,
    /// Файл, из которого берётся значок.
    pub icon: &'a Path,
}

pub fn create(link: &Path, target: &Path, description: &str) -> windows::core::Result<()> {
    create_with(link, &Shortcut { target, arguments: "", description, icon: target })
}

pub fn create_with(link: &Path, shortcut: &Shortcut<'_>) -> windows::core::Result<()> {
    unsafe {
        // Поток может уже быть инициализирован иначе — это не мешает созданию ярлыка.
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| {
            let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shell_link.SetPath(&HSTRING::from(shortcut.target.as_os_str()))?;
            if let Some(dir) = shortcut.target.parent() {
                shell_link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            if !shortcut.arguments.is_empty() {
                shell_link.SetArguments(&HSTRING::from(shortcut.arguments))?;
            }
            shell_link.SetDescription(&HSTRING::from(shortcut.description))?;
            shell_link.SetIconLocation(&HSTRING::from(shortcut.icon.as_os_str()), 0)?;
            let file: IPersistFile = shell_link.cast()?;
            file.Save(&HSTRING::from(link.as_os_str()), true)
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

/// Рабочий стол текущего пользователя (с учётом перенаправления папок).
pub fn user_desktop() -> Option<PathBuf> {
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const _));
        path.map(PathBuf::from)
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

        let game = dir.join("Игра.lnk");
        let shortcut = Shortcut {
            target: &target,
            arguments: r#"--launch "Warcraft III""#,
            description: "проверка",
            icon: &target,
        };
        create_with(&game, &shortcut).unwrap();
        assert!(std::fs::metadata(&game).unwrap().len() > 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_user_has_a_desktop() {
        assert!(user_desktop().is_some_and(|path| path.is_absolute()));
    }
}

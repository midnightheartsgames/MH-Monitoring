//! Запуск игр с параметрами и окно игры без рамки (PLAN.md §6/P10).
//!
//! Для старых игр без безрамочного режима: программа запускает игру в окне (у Warcraft III 1.26 —
//! `-window`), а когда это окно на переднем плане, снимает с него рамку и растягивает на монитор.
//! HUD тогда поверх игры, как в безрамочном режиме. В процесс игры ничего не попадает: меняется
//! только стиль её окна снаружи.
//!
//! Ярлык игры запускает `MH-Monitoring.exe --launch "<имя>"`: игра стартует, а оверлей — если
//! ещё не запущен.

use std::path::Path;

use crate::settings::GameProfile;

pub const LAUNCH_ARG: &str = "--launch";

/// Известные игры: имя файла в нижнем регистре → (параметры, файл окна).
const PRESETS: &[(&str, &str, Option<&str>)] = &[
    // Warcraft III до Reforged: загрузчики передают параметры в war3.exe и выходят.
    ("frozen throne.exe", "-window", Some("war3.exe")),
    ("warcraft iii.exe", "-window", Some("war3.exe")),
    ("war3.exe", "-window", None),
];

/// Новый профиль для файла игры. Для известных игр параметры подставляются сами.
pub fn profile_for(path: &Path, taken: impl Fn(&str) -> bool) -> GameProfile {
    let file = path.file_name().map(|name| name.to_string_lossy().to_lowercase());
    let preset = PRESETS.iter().find(|(name, ..)| Some(*name) == file.as_deref());
    let base = match preset {
        Some(_) => "Warcraft III".to_string(),
        None => name_from_path(&path.to_string_lossy()),
    };
    GameProfile {
        name: unique_name(&base, taken),
        path: path.display().to_string(),
        arguments: preset.map(|(_, arguments, _)| arguments.to_string()).unwrap_or_default(),
        borderless: preset.is_some(),
        window_exe: preset.and_then(|(_, _, window)| window.map(str::to_string)),
    }
}

/// Имя без кавычек и символов, запрещённых в именах файлов: из него делается имя ярлыка.
pub fn clean_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .filter(|c| !matches!(c, '"' | '<' | '>' | ':' | '/' | '\\' | '|' | '?' | '*'))
        .filter(|c| !c.is_control())
        .collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// Имя файла без расширения.
pub fn name_from_path(path: &str) -> String {
    let stem = Path::new(path.trim()).file_stem().map(|stem| stem.to_string_lossy().into_owned());
    stem.and_then(|stem| clean_name(&stem)).unwrap_or_else(|| "Игра".to_string())
}

/// `base`, а если занято — `base (2)`, `base (3)`… Регистр не различается: `taken` получает имя в
/// нижнем регистре.
pub fn unique_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(&base.to_lowercase()) {
        return base.to_string();
    }
    (2..)
        .map(|index| format!("{base} ({index})"))
        .find(|name| !taken(&name.to_lowercase()))
        .unwrap_or_else(|| base.to_string())
}

/// Параметры командной строки: пробелы разделяют, двойные кавычки объединяют.
pub fn split_arguments(text: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        arguments.push(current);
    }
    arguments
}

/// Относится ли процесс с файлом `image` к профилю: тот же файл окна в той же папке, что игра.
///
/// Папка обязательна: иначе без рамки остался бы, скажем, редактор карт из соседней игры с тем же
/// именем файла. А имя файла обязательно, потому что у игры в той же папке бывает и редактор.
pub fn matches(profile: &GameProfile, image: &Path) -> bool {
    let game = Path::new(&profile.path);
    let window_exe = profile
        .window_exe
        .clone()
        .or_else(|| game.file_name().map(|name| name.to_string_lossy().into_owned()));
    let same_file = match (window_exe, image.file_name()) {
        (Some(expected), Some(actual)) => expected.eq_ignore_ascii_case(&actual.to_string_lossy()),
        _ => false,
    };
    let same_dir = match (game.parent(), image.parent()) {
        (Some(a), Some(b)) => {
            a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
        }
        _ => false,
    };
    same_file && same_dir
}

/// Аргументы ярлыка игры.
pub fn launch_arguments(name: &str) -> String {
    format!("{LAUNCH_ARG} \"{name}\"")
}

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(windows)]
mod windows_impl {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use mh_platform::game_window::{self, Borderless};

    use super::*;
    use crate::diag;
    use crate::settings::Settings;

    /// Запускает игру из профиля и не ждёт её.
    pub fn launch(profile: &GameProfile) -> Result<(), String> {
        let path = Path::new(&profile.path);
        if !path.is_file() {
            return Err(format!("нет файла {}", path.display()));
        }
        let mut command = std::process::Command::new(path);
        command.args(split_arguments(&profile.arguments));
        if let Some(dir) = path.parent() {
            command.current_dir(dir);
        }
        command.spawn().map_err(|error| format!("{}: {error}", path.display()))?;
        diag::log(format!("игра запущена: {} {}", path.display(), profile.arguments));
        Ok(())
    }

    /// `--launch "<имя>"` из командной строки: запускает игру. Ошибку показывает окном —
    /// ярлык запускают без консоли.
    pub fn launch_from_arguments(arguments: &[String]) {
        let Some(index) = arguments.iter().position(|argument| argument == LAUNCH_ARG) else {
            return;
        };
        let Some(name) = arguments.get(index + 1) else { return };
        let settings = Settings::load(&crate::settings::default_path()).settings;
        let result = match settings.games.iter().find(|game| game.name.eq_ignore_ascii_case(name)) {
            Some(profile) => launch(profile),
            None => Err(format!("в настройках нет игры «{name}»")),
        };
        if let Err(error) = result {
            mh_platform::instance::message("MH Monitoring", &format!("Игра не запущена: {error}"));
        }
    }

    /// Ярлык игры на рабочем столе пользователя. Возвращает путь к ярлыку.
    pub fn create_desktop_shortcut(profile: &GameProfile) -> Result<PathBuf, String> {
        let desktop = mh_platform::shortcut::user_desktop()
            .ok_or_else(|| "не найден рабочий стол".to_string())?;
        let program = if crate::installer::installed_version().is_some() {
            crate::installer::installed_exe()
        } else {
            std::env::current_exe().map_err(|e| e.to_string())?
        };
        let link = desktop.join(format!("{} (MH Monitoring).lnk", profile.name));
        let arguments = launch_arguments(&profile.name);
        let description = format!("{} с оверлеем MH Monitoring", profile.name);
        let shortcut = mh_platform::shortcut::Shortcut {
            target: &program,
            arguments: &arguments,
            description: &description,
            icon: Path::new(&profile.path),
        };
        mh_platform::shortcut::create_with(&link, &shortcut).map_err(|e| e.to_string())?;
        Ok(link)
    }

    /// Как часто можно заново снимать рамку с того же окна: игра может возвращать её сама, и
    /// бороться с ней каждую секунду незачем.
    const RETRY_EVERY: Duration = Duration::from_secs(5);

    /// Следит за окном на переднем плане и снимает рамку с окон игр из профилей.
    #[derive(Default)]
    pub struct BorderlessWatcher {
        /// Последнее окно, с которым что-то делали, и когда.
        last: Option<(isize, Instant)>,
        /// Почему не вышло — для HUD.
        pub problem: Option<String>,
    }

    impl BorderlessWatcher {
        /// Вызывать раз в секунду.
        pub fn tick(&mut self, games: &[GameProfile]) {
            if !games.iter().any(|game| game.borderless) {
                self.problem = None;
                return;
            }
            let Some(window) = game_window::foreground() else { return };
            if window.pid == std::process::id() {
                return;
            }
            if let Some((hwnd, at)) = self.last
                && hwnd == window.hwnd
                && at.elapsed() < RETRY_EVERY
            {
                return;
            }
            let Some(image) = mh_platform::instance::image_path(window.pid) else {
                // Процесс от администратора путь не отдаёт — и окно его всё равно не изменить.
                return;
            };
            let Some(profile) = games.iter().find(|game| game.borderless && matches(game, &image))
            else {
                return;
            };
            match game_window::make_borderless(window.hwnd) {
                Ok(Borderless::Applied) => {
                    diag::log(format!("{}: окно без рамки на весь монитор", profile.name));
                    self.last = Some((window.hwnd, Instant::now()));
                    self.problem = None;
                }
                Ok(Borderless::AlreadyDone) => {
                    self.last = Some((window.hwnd, Instant::now()));
                    self.problem = None;
                }
                Ok(Borderless::NotAGameWindow) => {}
                Err(error) => {
                    self.last = Some((window.hwnd, Instant::now()));
                    let problem = if error.raw_os_error() == Some(5) {
                        format!(
                            "{}: окно не изменить — игра запущена от администратора",
                            profile.name
                        )
                    } else {
                        format!("{}: окно не изменено: {error}", profile.name)
                    };
                    if self.problem.as_ref() != Some(&problem) {
                        diag::log(&problem);
                    }
                    self.problem = Some(problem);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wc3() -> GameProfile {
        profile_for(Path::new(r"C:\Games\Warcraft III\Frozen Throne.exe"), |_| false)
    }

    #[test]
    fn warcraft_iii_gets_windowed_mode_and_war3_as_its_window() {
        let profile = wc3();
        assert_eq!(profile.name, "Warcraft III");
        assert_eq!(profile.arguments, "-window");
        assert!(profile.borderless);
        assert_eq!(profile.window_exe.as_deref(), Some("war3.exe"));
    }

    #[test]
    fn an_unknown_game_gets_its_file_name_and_no_arguments() {
        let profile = profile_for(Path::new(r"D:\Old\Diablo\Diablo.exe"), |name| name == "diablo");
        assert_eq!(profile.name, "Diablo (2)");
        assert_eq!(profile.arguments, "");
        assert!(!profile.borderless);
        assert_eq!(profile.window_exe, None);
    }

    #[test]
    fn the_window_must_be_the_right_file_in_the_game_folder() {
        let profile = wc3();
        assert!(matches(&profile, Path::new(r"c:\games\warcraft iii\WAR3.EXE")));
        assert!(!matches(&profile, Path::new(r"C:\Games\Warcraft III\worldedit.exe")));
        assert!(!matches(&profile, Path::new(r"C:\Other\war3.exe")));
        let mut direct = profile.clone();
        direct.window_exe = None;
        assert!(matches(&direct, Path::new(r"C:\Games\Warcraft III\Frozen Throne.exe")));
    }

    #[test]
    fn arguments_split_on_spaces_and_respect_quotes() {
        assert_eq!(split_arguments("  -window   -opengl "), ["-window", "-opengl"]);
        assert_eq!(
            split_arguments(r#"-loadfile "Maps\My Map.w3x" -x"#),
            ["-loadfile", r"Maps\My Map.w3x", "-x"]
        );
        assert_eq!(split_arguments(r#"-name """#), ["-name", ""]);
        assert!(split_arguments("   ").is_empty());
    }

    #[test]
    fn names_are_safe_for_file_names_and_unique() {
        assert_eq!(clean_name(r#" My "Game": 2 "#).as_deref(), Some("My Game 2"));
        assert_eq!(clean_name(r#""""#), None);
        assert_eq!(name_from_path(r"C:\x\Game.exe"), "Game");
        assert_eq!(unique_name("Game", |name| ["game", "game (2)"].contains(&name)), "Game (3)");
    }

    #[test]
    fn the_shortcut_quotes_the_name() {
        assert_eq!(launch_arguments("Warcraft III"), r#"--launch "Warcraft III""#);
        let parsed = split_arguments(&launch_arguments("Warcraft III"));
        assert_eq!(parsed, [LAUNCH_ARG, "Warcraft III"]);
    }
}

//! Дочерний процесс захвата за трейтом.
//!
//! Абстракция нужна ровно для одного: чтобы весь жизненный цикл сеанса — молчащий ребёнок, поток
//! только в stderr, отказ в доступе, неподдерживаемая схема, полсотни смен цели — проверялся
//! юнит-тестом, без запуска настоящего PresentMon и без прав администратора (PLAN.md §8).
//!
//! Настоящая реализация поверх `std::process` тонкая намеренно: всё, что можно решить до неё,
//! решено в [`super::command`], [`super::csv`] и [`super::diagnosis`].

use std::io::{self, Read};
use std::process::{Command, Stdio};

use super::command::CaptureCommand;

/// Запущенный процесс захвата.
pub trait CaptureChild: Send {
    /// Забирает stdout. Второй вызов отдаёт `None`.
    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>>;
    /// Забирает stderr. Второй вызов отдаёт `None`.
    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>>;
    /// Код возврата, если процесс уже завершился. Не блокирует.
    fn try_exit_code(&mut self) -> io::Result<Option<i32>>;
    /// Завершает процесс.
    ///
    /// На Windows это `TerminateProcess`, и ребёнок **не** успеет прибраться за собой
    /// (PLAN.md §2.2). Поэтому ETW-сессию гасим мы сами, по известному нам имени, а не
    /// надеемся, что это сделает он.
    fn kill(&mut self) -> io::Result<()>;
}

/// Чем запускать процесс захвата.
pub trait CaptureLauncher: Send + Sync {
    fn launch(&self, command: &CaptureCommand) -> io::Result<Box<dyn CaptureChild>>;
}

/// Что сделать с только что запущенным процессом, получив его PID.
pub type SpawnHook = std::sync::Arc<dyn Fn(u32) + Send + Sync>;

/// Запуск настоящего PresentMon.
#[derive(Default, Clone)]
pub struct ProcessLauncher {
    /// Вызывается сразу после запуска. На Windows через него ребёнок помещается в job object с
    /// `KILL_ON_JOB_CLOSE`: если приложение умрёт не своей смертью, `Drop` не выполнится, и
    /// только ядро сможет завершить PresentMon вместе с ним (`mh_platform::job`).
    pub on_spawn: Option<SpawnHook>,
}

impl std::fmt::Debug for ProcessLauncher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessLauncher")
            .field("on_spawn", &self.on_spawn.as_ref().map(|_| "…"))
            .finish()
    }
}

impl CaptureLauncher for ProcessLauncher {
    fn launch(&self, command: &CaptureCommand) -> io::Result<Box<dyn CaptureChild>> {
        let mut builder = Command::new(&command.executable);
        builder
            .args(&command.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // PresentMon — консольная программа. У оконного приложения консоли нет, и без этого флага
        // Windows открывает ей новое окно консоли. Окно забирает фокус, трекер уводит цель на него,
        // захват перезапускается, и так по кругу: в P4 окно мигало каждые несколько секунд, а игра
        // теряла фокус и проседала до 3 FPS.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            builder.creation_flags(CREATE_NO_WINDOW);
        }
        let child = builder.spawn()?;
        if let Some(hook) = &self.on_spawn {
            hook(child.id());
        }
        Ok(Box::new(SystemChild { child }))
    }
}

struct SystemChild {
    child: std::process::Child,
}

impl CaptureChild for SystemChild {
    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        self.child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        self.child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>)
    }

    fn try_exit_code(&mut self) -> io::Result<Option<i32>> {
        Ok(self.child.try_wait()?.map(|status| status.code().unwrap_or(-1)))
    }

    fn kill(&mut self) -> io::Result<()> {
        match self.child.kill() {
            Ok(()) => {
                // Пожинаем зомби сразу: иначе дескриптор процесса живёт до конца приложения,
                // а за пятьдесят смен цели их накопится пятьдесят.
                let _ = self.child.wait();
                Ok(())
            }
            // Процесс уже умер сам — это не ошибка, а обычный конец игры.
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
pub mod fake {
    //! Подставной ребёнок для тестов жизненного цикла.

    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Поток, который отдаёт заготовленные байты, а потом ведёт себя как сказано.
    pub struct ScriptedStream {
        chunks: std::vec::IntoIter<Vec<u8>>,
        current: std::io::Cursor<Vec<u8>>,
        /// После исчерпания кусков: `true` — молчать, пока не попросят остановиться (как труба
        /// живого процесса), `false` — сразу отдать конец потока.
        block_at_end: bool,
        stop: Arc<AtomicBool>,
    }

    impl Read for ScriptedStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                let read = self.current.read(buf)?;
                if read > 0 {
                    return Ok(read);
                }
                match self.chunks.next() {
                    Some(next) => self.current = std::io::Cursor::new(next),
                    None => {
                        if !self.block_at_end {
                            return Ok(0);
                        }
                        // Молчащий ребёнок: труба открыта, данных нет. Ровно тот случай, из-за
                        // которого чтение обязано жить на отдельном потоке (PLAN.md §2.8).
                        while !self.stop.load(Ordering::SeqCst) {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        return Ok(0);
                    }
                }
            }
        }
    }

    /// Сценарий поведения подставного процесса.
    #[derive(Default, Clone)]
    pub struct Script {
        pub stdout: Vec<Vec<u8>>,
        pub stderr: Vec<Vec<u8>>,
        /// Держать трубы открытыми после выдачи всех кусков.
        pub keep_running: bool,
        /// Код возврата, если процесс завершается сам.
        pub exit_code: Option<i32>,
    }

    impl Script {
        pub fn silent() -> Self {
            Self { keep_running: true, ..Default::default() }
        }

        pub fn stdout_text(text: &str) -> Self {
            Self { stdout: vec![utf16le(text)], keep_running: true, ..Default::default() }
        }

        pub fn stderr_only(text: &str, exit_code: i32) -> Self {
            Self { stderr: vec![utf16le(text)], exit_code: Some(exit_code), ..Default::default() }
        }
    }

    /// PresentMon пишет консольные потоки в UTF-16LE с BOM (PLAN.md §2.5) — подставной ребёнок
    /// обязан вести себя так же, иначе тесты проверяют не то, что бывает в жизни.
    pub fn utf16le(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    pub struct FakeChild {
        stdout: Option<Box<dyn Read + Send>>,
        stderr: Option<Box<dyn Read + Send>>,
        exit_code: Option<i32>,
        stop: Arc<AtomicBool>,
        killed: Arc<AtomicBool>,
    }

    impl CaptureChild for FakeChild {
        fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
            self.stdout.take()
        }

        fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
            self.stderr.take()
        }

        fn try_exit_code(&mut self) -> io::Result<Option<i32>> {
            Ok(if self.stop.load(Ordering::SeqCst) {
                self.exit_code.or(Some(0))
            } else {
                self.exit_code
            })
        }

        fn kill(&mut self) -> io::Result<()> {
            self.killed.store(true, Ordering::SeqCst);
            self.stop.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Запускальщик, отдающий заготовленных детей.
    pub struct FakeLauncher {
        script: Mutex<Script>,
        launches: AtomicUsize,
        /// Ошибка запуска вместо ребёнка — случай «exe нет».
        fail_with: Mutex<Option<io::ErrorKind>>,
        killed_flags: Mutex<Vec<Arc<AtomicBool>>>,
    }

    impl FakeLauncher {
        pub fn new(script: Script) -> Self {
            Self {
                script: Mutex::new(script),
                launches: AtomicUsize::new(0),
                fail_with: Mutex::new(None),
                killed_flags: Mutex::new(Vec::new()),
            }
        }

        pub fn failing(kind: io::ErrorKind) -> Self {
            let launcher = Self::new(Script::default());
            *launcher.fail_with.lock().unwrap() = Some(kind);
            launcher
        }

        pub fn launches(&self) -> usize {
            self.launches.load(Ordering::SeqCst)
        }

        /// Сколько запущенных детей было явно завершено. Проверка на утечку процессов.
        pub fn killed(&self) -> usize {
            self.killed_flags
                .lock()
                .unwrap()
                .iter()
                .filter(|flag| flag.load(Ordering::SeqCst))
                .count()
        }
    }

    impl CaptureLauncher for FakeLauncher {
        fn launch(&self, _command: &CaptureCommand) -> io::Result<Box<dyn CaptureChild>> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            if let Some(kind) = *self.fail_with.lock().unwrap() {
                return Err(io::Error::new(kind, "подставной отказ запуска"));
            }
            let script = self.script.lock().unwrap().clone();
            let stop = Arc::new(AtomicBool::new(false));
            let killed = Arc::new(AtomicBool::new(false));
            self.killed_flags.lock().unwrap().push(Arc::clone(&killed));

            let make = |chunks: Vec<Vec<u8>>| -> Box<dyn Read + Send> {
                Box::new(ScriptedStream {
                    chunks: chunks.into_iter(),
                    current: std::io::Cursor::new(Vec::new()),
                    block_at_end: script.keep_running,
                    stop: Arc::clone(&stop),
                })
            };

            Ok(Box::new(FakeChild {
                stdout: Some(make(script.stdout)),
                stderr: Some(make(script.stderr)),
                exit_code: script.exit_code,
                stop,
                killed,
            }))
        }
    }
}

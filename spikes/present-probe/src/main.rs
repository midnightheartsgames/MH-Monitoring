//! Эталонный презентер для проверки спайка `etw-frames`.
//!
//! Зачем он нужен. На простаивающем рабочем столе нет приложения, которое непрерывно вызывает
//! `IDXGISwapChain::Present`, а без такого источника невозможно отличить «потребитель ETW
//! сломан» от «презентить просто нечего». Этот бинарник создаёт настоящую цепочку обмена D3D11
//! и презентит в цикле, **сам считая свои кадры**. Его число Present — эталон, с которым
//! сверяется то, что насчитал `etw-frames` по событиям ETW.
//!
//! Прав администратора не требует: он ничего не измеряет, он только рисует.
//!
//! ```text
//! present-probe.exe --seconds 20            # с вертикальной синхронизацией, ~частота монитора
//! present-probe.exe --seconds 20 --vsync 0  # без неё, сотни кадров в секунду
//! ```

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDeviceAndSwapChain, ID3D11Device,
    ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_DESC, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGISwapChain,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DispatchMessageW, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, WINDOW_EX_STYLE,
    WM_DESTROY, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{PCWSTR, w};

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;

struct Args {
    seconds: u64,
    vsync: u32,
    /// Верхняя граница частоты кадров, 0 — без ограничения. Нужна, чтобы проверять
    /// потребителя ETW на разных потоках событий, а не только на максимальном.
    fps_cap: u32,
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1);
    let mut args = Args { seconds: 20, vsync: 1, fps_cap: 0 };
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--seconds" => {
                let value = argv.next().ok_or("--seconds требует значение")?;
                args.seconds = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "--vsync" => {
                let value = argv.next().ok_or("--vsync требует значение")?;
                args.vsync = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "--fps" => {
                let value = argv.next().ok_or("--fps требует значение")?;
                args.fps_cap = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "-h" | "--help" => {
                println!(
                    "Эталонный D3D11-презентер.\n\n\
                     --seconds <N>   сколько презентить (по умолчанию 20)\n\
                     --vsync <0|1>   интервал синхронизации Present (по умолчанию 1)
                     --fps <N>       ограничить частоту кадров (0 — без ограничения)"
                );
                std::process::exit(0);
            }
            other => return Err(format!("неизвестный аргумент: {other}")),
        }
    }
    Ok(args)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESTROY {
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn create_window() -> windows::core::Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("MHMonitorPresentProbe");
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        // Ноль означает, что класс зарегистрировать не удалось; для одноразового пробника
        // достаточно проверить это здесь.
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            PCWSTR(w!("MH Monitoring — эталонный презентер").as_ptr()),
            WS_OVERLAPPEDWINDOW,
            64,
            64,
            WIDTH as i32,
            HEIGHT as i32,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        // Показываем без отбора фокуса: пробник не должен мешать тому, что делает пользователь.
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Ok(hwnd)
    }
}

fn main() -> windows::core::Result<()> {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("ошибка аргументов: {message}");
            std::process::exit(2);
        }
    };

    let hwnd = create_window()?;

    let desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Width: WIDTH,
            Height: HEIGHT,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            ..Default::default()
        },
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        OutputWindow: hwnd,
        Windowed: true.into(),
        SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
        ..Default::default()
    };

    let mut swap_chain: Option<IDXGISwapChain> = None;
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe {
        D3D11CreateDeviceAndSwapChain(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            None,
            D3D11_SDK_VERSION,
            Some(&desc),
            Some(&mut swap_chain),
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let swap_chain = swap_chain.expect("D3D11CreateDeviceAndSwapChain вернул успех без цепочки");
    let device = device.expect("D3D11CreateDeviceAndSwapChain вернул успех без устройства");
    let context = context.expect("D3D11CreateDeviceAndSwapChain вернул успех без контекста");

    let back_buffer: ID3D11Texture2D = unsafe { swap_chain.GetBuffer(0)? };
    let mut render_target: Option<ID3D11RenderTargetView> = None;
    unsafe { device.CreateRenderTargetView(&back_buffer, None, Some(&mut render_target))? };
    let render_target = render_target.expect("CreateRenderTargetView вернул успех без представления");

    println!("PID {}", unsafe { GetCurrentProcessId() });
    println!(
        "презентю {} с, интервал синхронизации {}, ограничение {} FPS",
        args.seconds,
        args.vsync,
        if args.fps_cap == 0 { "без".to_string() } else { args.fps_cap.to_string() }
    );

    let deadline = Duration::from_secs(args.seconds);
    let started = Instant::now();
    let mut presents: u64 = 0;
    let mut message = MSG::default();

    while started.elapsed() < deadline {
        // Окно должно оставаться отзывчивым, иначе система сочтёт его зависшим и подменит
        // изображение — а это уже другой путь вывода, не наш Present.
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        // Цвет плавно меняется: так на глаз видно, что кадры действительно идут.
        let phase = started.elapsed().as_secs_f32();
        let color = [
            0.5 + 0.5 * phase.sin(),
            0.5 + 0.5 * (phase * 0.7).sin(),
            0.5 + 0.5 * (phase * 1.3).sin(),
            1.0,
        ];
        unsafe {
            context.ClearRenderTargetView(&render_target, &color);
            // Возврат Present игнорируем намеренно: DXGI_STATUS_OCCLUDED — это успех с
            // предупреждением, и для счёта событий ETW он ничем не отличается от обычного кадра.
            let _ = swap_chain.Present(args.vsync, windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0));
        }
        presents += 1;

        if args.fps_cap > 0 {
            // Целевой момент следующего кадра считаем от начала замера, а не от «сейчас»:
            // так ошибка сна не накапливается и средняя частота держится на заданной.
            let target = Duration::from_secs_f64(presents as f64 / args.fps_cap as f64);
            if let Some(wait) = target.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    println!("\n--- эталон ---");
    println!("Present вызван  : {presents}");
    println!("длительность    : {elapsed:.2} с");
    println!("FPS             : {:.1}", presents as f64 / elapsed);
    Ok(())
}

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    env,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalSize, Manager, Monitor, PhysicalPosition, State, WebviewWindow,
    WindowEvent,
};

#[cfg(target_os = "windows")]
mod windows_integration;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const FLOATING_CARD_WIDTH: f64 = 276.0;
const FLOATING_CARD_HEIGHT: f64 = 150.0;
const FLOATING_ORB_SIZE: f64 = 68.0;
const FLOATING_ORB_HOVER_WIDTH: f64 = 230.0;
const FLOATING_ORB_HOVER_HEIGHT: f64 = 68.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateWindow {
    used_percent: f64,
    window_duration_mins: u64,
    resets_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageSnapshot {
    email: Option<String>,
    plan_type: Option<String>,
    primary: Option<RateWindow>,
    secondary: Option<RateWindow>,
    credit_balance: Option<String>,
    has_credits: bool,
    unlimited: bool,
    reset_credits: u64,
    fetched_at: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct SavedPosition {
    x: i32,
    y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PersistedState {
    codex_path: Option<PathBuf>,
    last_usage: Option<UsageSnapshot>,
    main_position: Option<SavedPosition>,
    #[serde(alias = "floatingPosition")]
    floating_position: Option<SavedPosition>,
    floating_card_position: Option<SavedPosition>,
    floating_orb_position: Option<SavedPosition>,
    floating_visible: bool,
    floating_pinned: bool,
    floating_opacity: f64,
    floating_always_on_top: bool,
    floating_style: String,
    floating_orb_expand_direction: String,
    display_mode: String,
    theme: String,
    proxy_mode: String,
    proxy_address: String,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            codex_path: None,
            last_usage: None,
            main_position: None,
            floating_position: None,
            floating_card_position: None,
            floating_orb_position: None,
            floating_visible: false,
            floating_pinned: false,
            floating_opacity: 0.92,
            floating_always_on_top: true,
            floating_style: "card".to_owned(),
            floating_orb_expand_direction: "auto".to_owned(),
            display_mode: "available".to_owned(),
            theme: "lime".to_owned(),
            proxy_mode: "system".to_owned(),
            proxy_address: String::new(),
        }
    }
}

struct AppState {
    file_path: PathBuf,
    data: Mutex<PersistedState>,
    save_sender: mpsc::Sender<PersistedState>,
    floating_pinned: Arc<AtomicBool>,
    floating_orb: Arc<AtomicBool>,
    floating_orb_dragging: AtomicBool,
    floating_orb_expanded: AtomicBool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FloatingSettings {
    visible: bool,
    pinned: bool,
    opacity: f64,
    always_on_top: bool,
    style: String,
    orb_expand_direction: String,
    display_mode: String,
    theme: String,
    proxy_mode: String,
    proxy_address: String,
    data_directory: String,
}

#[derive(Debug, Clone)]
struct NetworkSettings {
    mode: String,
    address: String,
}

#[derive(Debug)]
struct CodexLauncher {
    source_path: PathBuf,
    program: PathBuf,
    args: Vec<OsString>,
}

fn load_state(path: &Path) -> PersistedState {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

fn executable_data_dir() -> Result<PathBuf, String> {
    let executable =
        env::current_exe().map_err(|error| format!("无法定位应用程序目录：{error}"))?;
    let directory = executable
        .parent()
        .ok_or_else(|| "无法定位应用程序目录".to_owned())?;
    Ok(directory.join("data"))
}

fn migrate_legacy_state(legacy_file: &Path, target_file: &Path) {
    if !target_file.exists() && legacy_file.is_file() {
        let _ = fs::copy(legacy_file, target_file);
    }
}

fn save_state(file_path: &Path, snapshot: &PersistedState) -> Result<(), String> {
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建 data 目录：{error}"))?;
    }
    let contents = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| format!("无法序列化本地状态：{error}"))?;
    fs::write(file_path, contents).map_err(|error| format!("无法保存本地状态：{error}"))
}

fn update_state(state: &AppState, update: impl FnOnce(&mut PersistedState)) -> Result<(), String> {
    let snapshot = {
        let mut data = state
            .data
            .lock()
            .map_err(|_| "本地状态暂时不可用".to_owned())?;
        update(&mut data);
        data.clone()
    };
    state
        .save_sender
        .send(snapshot)
        .map_err(|_| "后台存储线程已停止".to_owned())
}

fn start_state_writer(file_path: PathBuf) -> mpsc::Sender<PersistedState> {
    let (sender, receiver) = mpsc::channel::<PersistedState>();
    thread::spawn(move || {
        while let Ok(mut latest) = receiver.recv() {
            while let Ok(newer) = receiver.try_recv() {
                latest = newer;
            }
            let _ = save_state(&file_path, &latest);
        }
    });
    sender
}

fn add_candidate(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    if path.is_file() && !candidates.iter().any(|item| item == &path) {
        candidates.push(path);
    }
}

fn launcher_from_path(path: PathBuf) -> CodexLauncher {
    let script = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("cmd") || value.eq_ignore_ascii_case("bat")
        });
    if script {
        let command = format!(
            "\"{}\" app-server --listen stdio://",
            path.to_string_lossy()
        );
        CodexLauncher {
            source_path: path,
            program: PathBuf::from("cmd.exe"),
            args: ["/D", "/S", "/C"]
                .into_iter()
                .map(OsString::from)
                .chain([OsString::from(command)])
                .collect(),
        }
    } else {
        CodexLauncher {
            source_path: path.clone(),
            program: path,
            args: ["app-server", "--listen", "stdio://"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        }
    }
}

#[cfg(target_os = "windows")]
fn find_codex_launcher(cached: Option<&Path>) -> Result<CodexLauncher, String> {
    let mut candidates = Vec::new();
    if let Some(path) = cached {
        add_candidate(&mut candidates, path.to_path_buf());
    }
    if let Some(path) = env::var_os("CODEX_QUOTA_CODEX_PATH") {
        add_candidate(&mut candidates, PathBuf::from(path));
    }
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        let bin = PathBuf::from(local).join("OpenAI/Codex/bin");
        add_candidate(&mut candidates, bin.join("codex.exe"));
        if let Ok(entries) = fs::read_dir(&bin) {
            let mut paths: Vec<_> = entries
                .flatten()
                .map(|entry| entry.path().join("codex.exe"))
                .filter(|path| path.is_file())
                .collect();
            paths.sort_by_key(|path| {
                fs::metadata(path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
            });
            paths.reverse();
            for path in paths {
                add_candidate(&mut candidates, path);
            }
        }
    }
    if let Some(value) = env::var_os("PATH") {
        for directory in env::split_paths(&value) {
            add_candidate(&mut candidates, directory.join("codex.exe"));
            add_candidate(&mut candidates, directory.join("codex.cmd"));
            add_candidate(&mut candidates, directory.join("codex.bat"));
        }
    }
    if let Some(app_data) = env::var_os("APPDATA") {
        add_candidate(
            &mut candidates,
            PathBuf::from(app_data).join("npm/codex.cmd"),
        );
    }
    candidates.into_iter().next().map(launcher_from_path).ok_or_else(||
        "未找到 Codex。请先安装 Codex 桌面应用或 Codex CLI，并登录 ChatGPT 账号；也可以通过 CODEX_QUOTA_CODEX_PATH 指定 codex.exe。".to_owned())
}

#[cfg(not(target_os = "windows"))]
fn find_codex_launcher(cached: Option<&Path>) -> Result<CodexLauncher, String> {
    Ok(launcher_from_path(
        cached.unwrap_or_else(|| Path::new("codex")).to_path_buf(),
    ))
}

fn parse_window(value: Option<&Value>) -> Option<RateWindow> {
    let value = value?;
    Some(RateWindow {
        used_percent: value.get("usedPercent")?.as_f64()?,
        window_duration_mins: value.get("windowDurationMins")?.as_u64()?,
        resets_at: value.get("resetsAt")?.as_u64()?,
    })
}

fn parse_usage_responses(
    rate: &Value,
    account: Option<&Value>,
    fetched_at: u64,
) -> Result<UsageSnapshot, String> {
    let account_result = account.and_then(|value| value.get("result"));
    let account_info = account_result.and_then(|value| value.get("account"));
    if account_result
        .and_then(|value| value.get("requiresOpenaiAuth"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && account_info.is_none_or(Value::is_null)
    {
        return Err("这台电脑上的 Codex 尚未登录。请先打开 Codex 登录 ChatGPT 账号，或在终端执行 codex login，然后返回应用重试。".to_owned());
    }
    if account_info
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("apiKey"))
    {
        return Err("当前 Codex 使用 API Key 登录，无法读取 ChatGPT 订阅额度。请改用 ChatGPT 账号登录 Codex。".to_owned());
    }
    if let Some(error) = rate.get("error") {
        return Err(format!(
            "Codex 无法读取额度：{}",
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("未知错误")
        ));
    }
    let result = rate.get("result").ok_or("Codex 没有返回额度结果")?;
    let limits = result
        .pointer("/rateLimitsByLimitId/codex")
        .or_else(|| result.get("rateLimits"))
        .ok_or("Codex 没有返回可识别的额度数据")?;
    let credits = limits.get("credits");
    Ok(UsageSnapshot {
        email: account_info
            .and_then(|value| value.get("email"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        plan_type: limits
            .get("planType")
            .and_then(Value::as_str)
            .or_else(|| {
                account_info
                    .and_then(|value| value.get("planType"))
                    .and_then(Value::as_str)
            })
            .map(str::to_owned),
        primary: parse_window(limits.get("primary")),
        secondary: parse_window(limits.get("secondary")),
        credit_balance: credits
            .and_then(|value| value.get("balance"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        has_credits: credits
            .and_then(|value| value.get("hasCredits"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        unlimited: credits
            .and_then(|value| value.get("unlimited"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        reset_credits: result
            .pointer("/rateLimitResetCredits/availableCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        fetched_at,
    })
}

fn configure_proxy(command: &mut Command, network: &NetworkSettings) -> Result<(), String> {
    const PROXY_VARS: [&str; 6] = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ];
    match network.mode.as_str() {
        "system" => {}
        "none" => {
            for key in PROXY_VARS {
                command.env_remove(key);
            }
            command.env("NO_PROXY", "*").env("no_proxy", "*");
        }
        "custom" => {
            let address = normalize_proxy_address(&network.address)?;
            for key in PROXY_VARS {
                command.env(key, &address);
            }
            command.env("NO_PROXY", "localhost,127.0.0.1,::1");
        }
        _ => return Err("代理模式无效".to_owned()),
    }
    Ok(())
}

fn normalize_proxy_address(address: &str) -> Result<String, String> {
    let address = address.trim();
    if address.is_empty() {
        return Err("请填写本地代理地址，例如 http://127.0.0.1:7890".to_owned());
    }
    let normalized = if address.contains("://") {
        address.to_owned()
    } else {
        format!("http://{address}")
    };
    if !(normalized.starts_with("http://")
        || normalized.starts_with("https://")
        || normalized.starts_with("socks5://"))
    {
        return Err("代理地址仅支持 http、https 或 socks5".to_owned());
    }
    Ok(normalized)
}

fn query_codex(
    launcher: CodexLauncher,
    network: &NetworkSettings,
) -> Result<(UsageSnapshot, PathBuf), String> {
    let resolved = launcher.source_path.clone();
    let mut command = Command::new(&launcher.program);
    command
        .args(&launcher.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    configure_proxy(&mut command, network)?;
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 Codex（{}）：{error}", launcher.program.display()))?;
    let mut stdin = child.stdin.take().ok_or("无法连接 Codex 输入流")?;
    let stdout = child.stdout.take().ok_or("无法连接 Codex 输出流")?;
    let (sender, receiver) = mpsc::channel::<Value>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        }
    });
    for message in [
        json!({"method":"initialize","id":1,"params":{"clientInfo":{"name":"codex_quota","title":"Codex 额度","version":env!("CARGO_PKG_VERSION")}}}),
        json!({"method":"initialized","params":{}}),
        json!({"method":"account/rateLimits/read","id":2,"params":{}}),
        json!({"method":"account/read","id":3,"params":{"refreshToken":false}}),
    ] {
        writeln!(stdin, "{message}").map_err(|error| format!("向 Codex 发送请求失败：{error}"))?;
    }
    let (mut rate, mut account) = (None, None);
    let deadline = SystemTime::now() + Duration::from_secs(30);
    while SystemTime::now() < deadline && (rate.is_none() || account.is_none()) {
        match receiver.recv_timeout(Duration::from_millis(500)) {
            Ok(message) => match message.get("id").and_then(Value::as_i64) {
                Some(2) => rate = Some(message),
                Some(3) => account = Some(message),
                _ => {}
            },
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    drop(stdin);
    let _ = child.kill();
    let fetched_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let rate =
        rate.ok_or("Codex App Server 没有返回额度数据。请确认 Codex 已更新到最新版本并已登录。")?;
    Ok((
        parse_usage_responses(&rate, account.as_ref(), fetched_at)?,
        resolved,
    ))
}

fn read_codex_usage(
    cached: Option<&Path>,
    network: NetworkSettings,
) -> Result<(UsageSnapshot, PathBuf), String> {
    let launcher = find_codex_launcher(cached)?;
    let attempted_path = launcher.source_path.clone();
    match query_codex(launcher, &network) {
        Ok(result) => Ok(result),
        Err(cached_error) if cached.is_some_and(|path| path == attempted_path) => {
            // The cached executable can still exist after Codex updates while no longer
            // being the correct app-server. Rescan without it and persist the replacement
            // after the retry succeeds.
            let fresh_launcher = find_codex_launcher(None)?;
            if fresh_launcher.source_path == attempted_path {
                return Err(cached_error);
            }
            query_codex(fresh_launcher, &network).map_err(|fresh_error| {
                format!("缓存的 Codex 路径已失效，自动重新检索后仍无法读取额度：{fresh_error}")
            })
        }
        Err(error) => Err(error),
    }
}

#[tauri::command]
async fn get_codex_usage(state: State<'_, AppState>) -> Result<UsageSnapshot, String> {
    let (cached, network) = {
        let data = state
            .data
            .lock()
            .map_err(|_| "本地状态暂时不可用".to_owned())?;
        (
            data.codex_path.clone(),
            NetworkSettings {
                mode: data.proxy_mode.clone(),
                address: data.proxy_address.clone(),
            },
        )
    };
    let (usage, path) =
        tauri::async_runtime::spawn_blocking(move || read_codex_usage(cached.as_deref(), network))
            .await
            .map_err(|error| format!("额度后台线程异常：{error}"))??;
    update_state(&state, |data| {
        data.codex_path = Some(path);
        data.last_usage = Some(usage.clone());
    })?;
    Ok(usage)
}

#[tauri::command]
fn get_cached_usage(state: State<'_, AppState>) -> Result<Option<UsageSnapshot>, String> {
    Ok(state
        .data
        .lock()
        .map_err(|_| "本地状态暂时不可用".to_owned())?
        .last_usage
        .clone())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchAction {
    ShowMain,
    ToggleFloating,
    TogglePin,
    Quit,
}

fn launch_action(args: &[String]) -> LaunchAction {
    if args.iter().any(|value| value == "--toggle-floating") {
        LaunchAction::ToggleFloating
    } else if args.iter().any(|value| value == "--toggle-pin") {
        LaunchAction::TogglePin
    } else if args.iter().any(|value| value == "--quit") {
        LaunchAction::Quit
    } else {
        LaunchAction::ShowMain
    }
}

fn handle_launch_action(app: &AppHandle, args: &[String]) {
    match launch_action(args) {
        LaunchAction::ShowMain => show_main_window(app),
        LaunchAction::ToggleFloating => {
            let _ = toggle_floating(app);
        }
        LaunchAction::TogglePin => {
            let _ = toggle_pin(app);
        }
        LaunchAction::Quit => app.exit(0),
    }
}

fn set_floating_visible(app: &AppHandle, visible: bool) -> Result<bool, String> {
    let window = app.get_webview_window("floating").ok_or("悬浮窗尚未创建")?;
    let pinned = app
        .state::<AppState>()
        .data
        .lock()
        .map_err(|_| "本地状态暂时不可用".to_owned())?
        .floating_pinned;
    if visible {
        window
            .set_always_on_top(true)
            .map_err(|error| error.to_string())?;
        window.show().map_err(|error| error.to_string())?;
        if !pinned {
            window.set_focus().map_err(|error| error.to_string())?;
        }
    } else {
        window.hide().map_err(|error| error.to_string())?;
    }
    update_state(&app.state::<AppState>(), |data| {
        data.floating_visible = visible
    })?;
    Ok(visible)
}

fn toggle_floating(app: &AppHandle) -> Result<bool, String> {
    let window = app.get_webview_window("floating").ok_or("悬浮窗尚未创建")?;
    set_floating_visible(
        app,
        !window.is_visible().map_err(|error| error.to_string())?,
    )
}

fn is_floating_pin_hit(
    cursor_x: i32,
    cursor_y: i32,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
) -> bool {
    let scale_x = width as f64 / 308.0;
    let scale_y = height as f64 / 174.0;
    let button_left = left + (210.0 * scale_x).round() as i32;
    let button_right = left + (252.0 * scale_x).round() as i32;
    let button_bottom = top + (38.0 * scale_y).round() as i32;
    cursor_x >= button_left
        && cursor_x <= button_right
        && cursor_y >= top
        && cursor_y <= button_bottom
}

#[cfg(target_os = "windows")]
fn is_screenshot_capture_window(hwnd: windows_sys::Win32::Foundation::HWND) -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::WindowsAndMessaging::GetWindowThreadProcessId,
    };

    if hwnd.is_null() {
        return false;
    }
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) };
    if process_id == 0 {
        return false;
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return false;
    }

    let mut path = [0u16; 512];
    let mut length = path.len() as u32;
    let queried =
        unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) != 0 };
    unsafe { CloseHandle(process) };
    if !queried {
        return false;
    }

    let path = String::from_utf16_lossy(&path[..length as usize]);
    let executable = path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        executable.as_str(),
        "snippingtool.exe"
            | "screenclippinghost.exe"
            | "screensketch.exe"
            | "sharex.exe"
            | "greenshot.exe"
            | "lightshot.exe"
            | "snagit.exe"
            | "snagitcapture.exe"
            | "picpick.exe"
            | "faststonecapture.exe"
            | "winsnap.exe"
    )
}

#[cfg(target_os = "windows")]
fn start_click_through_controller(
    window: WebviewWindow,
    pinned: Arc<AtomicBool>,
    orb_mode: Arc<AtomicBool>,
) {
    use windows_sys::Win32::{
        Foundation::{POINT, RECT},
        UI::WindowsAndMessaging::{
            GetCursorPos, GetForegroundWindow, GetWindow, GetWindowRect, IsWindowVisible,
            SetWindowPos, GW_HWNDPREV, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
            SWP_NOOWNERZORDER, SWP_NOSIZE,
        },
    };

    let Ok(raw_hwnd) = window.hwnd() else {
        return;
    };
    let hwnd_value = raw_hwnd.0 as isize;
    thread::spawn(move || {
        let hwnd = hwnd_value as windows_sys::Win32::Foundation::HWND;
        let mut last_passthrough = false;
        let mut last_foreground = std::ptr::null_mut();
        let mut screenshot_overlay_active = false;
        let mut last_topmost_check = Instant::now();
        loop {
            let mut point = POINT { x: 0, y: 0 };
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            let valid =
                unsafe { GetCursorPos(&mut point) != 0 && GetWindowRect(hwnd, &mut rect) != 0 };
            if !valid {
                break;
            }

            let foreground = unsafe { GetForegroundWindow() };
            if foreground != last_foreground {
                last_foreground = foreground;
                let capture_active = is_screenshot_capture_window(foreground);
                if capture_active != screenshot_overlay_active {
                    screenshot_overlay_active = capture_active;
                    unsafe {
                        SetWindowPos(
                            hwnd,
                            if capture_active {
                                HWND_NOTOPMOST
                            } else {
                                HWND_TOPMOST
                            },
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                        );
                    }
                }
            }

            // Reassert the topmost order once per second, except while a
            // screenshot tool is active. Never activate the window here.
            if !screenshot_overlay_active && last_topmost_check.elapsed() >= Duration::from_secs(1)
            {
                last_topmost_check = Instant::now();
                if unsafe { IsWindowVisible(hwnd) != 0 && !GetWindow(hwnd, GW_HWNDPREV).is_null() }
                {
                    unsafe {
                        SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                        );
                    }
                }
            }

            let over_pin = is_floating_pin_hit(
                point.x,
                point.y,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
            );
            let passthrough =
                pinned.load(Ordering::Relaxed) && !orb_mode.load(Ordering::Relaxed) && !over_pin;
            if passthrough != last_passthrough {
                let _ = window.set_ignore_cursor_events(passthrough);
                last_passthrough = passthrough;
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
}

#[cfg(not(target_os = "windows"))]
fn start_click_through_controller(_: WebviewWindow, _: Arc<AtomicBool>, _: Arc<AtomicBool>) {}

#[tauri::command]
fn set_floating_window(app: AppHandle, visible: bool) -> Result<bool, String> {
    set_floating_visible(&app, visible)
}

#[tauri::command]
fn get_floating_settings(state: State<'_, AppState>) -> Result<FloatingSettings, String> {
    let data = state
        .data
        .lock()
        .map_err(|_| "本地状态暂时不可用".to_owned())?;
    Ok(FloatingSettings {
        visible: data.floating_visible,
        pinned: data.floating_pinned,
        opacity: data.floating_opacity,
        always_on_top: data.floating_always_on_top,
        style: data.floating_style.clone(),
        orb_expand_direction: data.floating_orb_expand_direction.clone(),
        display_mode: data.display_mode.clone(),
        theme: data.theme.clone(),
        proxy_mode: data.proxy_mode.clone(),
        proxy_address: data.proxy_address.clone(),
        data_directory: state
            .file_path
            .parent()
            .unwrap_or(Path::new("data"))
            .display()
            .to_string(),
    })
}

fn floating_logical_size(style: &str, expanded: bool) -> LogicalSize<f64> {
    match (style, expanded) {
        ("orb", true) => LogicalSize::new(FLOATING_ORB_HOVER_WIDTH, FLOATING_ORB_HOVER_HEIGHT),
        ("orb", false) => LogicalSize::new(FLOATING_ORB_SIZE, FLOATING_ORB_SIZE),
        _ => LogicalSize::new(FLOATING_CARD_WIDTH, FLOATING_CARD_HEIGHT),
    }
}

fn edge_side(
    position_x: i32,
    window_width: u32,
    monitor_left: i32,
    monitor_width: u32,
) -> &'static str {
    let window_center = position_x as i64 + window_width as i64 / 2;
    let monitor_center = monitor_left as i64 + monitor_width as i64 / 2;
    if window_center <= monitor_center {
        "left"
    } else {
        "right"
    }
}

fn clamp_vertical(y: i32, height: u32, monitor_top: i32, monitor_height: u32, margin: i32) -> i32 {
    let minimum = monitor_top + margin;
    let maximum = monitor_top + monitor_height as i32 - height as i32 - margin;
    y.clamp(minimum, maximum.max(minimum))
}

fn normalize_orb_expand_direction(direction: &str) -> Option<&'static str> {
    match direction {
        "auto" => Some("auto"),
        "left" => Some("left"),
        "right" => Some("right"),
        _ => None,
    }
}

fn orb_side_for_direction(direction: &str, automatic_side: &'static str) -> &'static str {
    match direction {
        // The panel opens to the left, so the orb stays on the right side.
        "left" => "right",
        // The panel opens to the right, so the orb stays on the left side.
        "right" => "left",
        _ => automatic_side,
    }
}

fn orb_window_x(orb_left: i32, orb_width: i32, margin: i32, side: &str, window_width: u32) -> i32 {
    if side == "right" {
        orb_left + orb_width + margin - window_width as i32
    } else {
        orb_left - margin
    }
}

fn orb_anchor_x_from_window(window_x: i32, window_width: u32, orb_size: i32, side: &str) -> i32 {
    if side == "right" {
        window_x + window_width as i32 - orb_size
    } else {
        window_x
    }
}

fn monitor_for_orb_anchor(
    window: &WebviewWindow,
    anchor: SavedPosition,
) -> Result<Monitor, String> {
    let monitors = window
        .available_monitors()
        .map_err(|error| error.to_string())?;
    if let Some(monitor) = monitors.into_iter().find(|monitor| {
        let half_orb = (FLOATING_ORB_SIZE * monitor.scale_factor() / 2.0).round() as i32;
        let center_x = anchor.x + half_orb;
        let center_y = anchor.y + half_orb;
        center_x >= monitor.position().x
            && center_x < monitor.position().x + monitor.size().width as i32
            && center_y >= monitor.position().y
            && center_y < monitor.position().y + monitor.size().height as i32
    }) {
        return Ok(monitor);
    }
    window
        .current_monitor()
        .map_err(|error| error.to_string())?
        .or(window
            .primary_monitor()
            .map_err(|error| error.to_string())?)
        .ok_or("找不到当前显示器".to_owned())
}

fn orb_layout_side(window: &WebviewWindow, direction: &str) -> Result<&'static str, String> {
    let monitor = window
        .current_monitor()
        .map_err(|error| error.to_string())?
        .or(window
            .primary_monitor()
            .map_err(|error| error.to_string())?)
        .ok_or("找不到当前显示器")?;
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let size = window.outer_size().map_err(|error| error.to_string())?;
    Ok(orb_side_for_direction(
        direction,
        edge_side(
            position.x,
            size.width,
            monitor.position().x,
            monitor.size().width,
        ),
    ))
}

fn orb_layout_side_for_anchor(
    window: &WebviewWindow,
    direction: &str,
    anchor: SavedPosition,
) -> Result<&'static str, String> {
    let monitor = monitor_for_orb_anchor(window, anchor)?;
    let scale = monitor.scale_factor();
    let collapsed_width = (FLOATING_ORB_SIZE * scale).round() as u32;
    let margin = (6.0 * scale).round() as i32;
    let left = monitor.position().x + margin;
    let right =
        monitor.position().x + monitor.size().width as i32 - collapsed_width as i32 - margin;
    let threshold = (28.0 * scale).round() as i32;
    if (anchor.x - left).abs() <= threshold {
        return Ok("left");
    }
    if (anchor.x - right).abs() <= threshold {
        return Ok("right");
    }
    Ok(orb_side_for_direction(
        direction,
        edge_side(
            anchor.x,
            collapsed_width,
            monitor.position().x,
            monitor.size().width,
        ),
    ))
}

fn orb_anchor_from_window(
    window: &WebviewWindow,
    state: &AppState,
    position: SavedPosition,
) -> Option<SavedPosition> {
    let current_monitor = window
        .current_monitor()
        .ok()?
        .or(window.primary_monitor().ok()?)?;
    let size = window.outer_size().ok()?;
    let (direction, previous_anchor) = {
        let data = state.data.lock().ok()?;
        (
            data.floating_orb_expand_direction.clone(),
            data.floating_orb_position,
        )
    };
    let collapsed_now = size.width
        <= (FLOATING_ORB_SIZE * current_monitor.scale_factor()).round() as u32
            + (12.0 * current_monitor.scale_factor()).round() as u32;
    let monitor = if collapsed_now {
        current_monitor
    } else {
        previous_anchor
            .and_then(|anchor| monitor_for_orb_anchor(window, anchor).ok())
            .unwrap_or(current_monitor)
    };
    let scale = monitor.scale_factor();
    let current_side = if collapsed_now {
        orb_layout_side(window, &direction).ok()?
    } else if let Some(previous_anchor) = previous_anchor {
        orb_layout_side_for_anchor(window, &direction, previous_anchor).ok()?
    } else {
        orb_layout_side(window, &direction).ok()?
    };
    let margin = (6.0 * scale).round() as i32;
    let orb_width = (56.0 * scale).round() as i32;
    let anchor_x =
        orb_anchor_x_from_window(position.x, size.width, orb_width + 2 * margin, current_side);
    Some(SavedPosition {
        x: anchor_x,
        y: position.y,
    })
}

fn orb_edge_anchor(
    window: &WebviewWindow,
    state: &AppState,
    position: SavedPosition,
) -> Option<(SavedPosition, &'static str)> {
    let anchor = orb_anchor_from_window(window, state, position)?;
    let monitor = monitor_for_orb_anchor(window, anchor).ok()?;
    let scale = monitor.scale_factor();
    let margin = (6.0 * scale).round() as i32;
    let collapsed_width = (FLOATING_ORB_SIZE * scale).round() as u32;
    let left_x = monitor.position().x + margin;
    let right_x =
        monitor.position().x + monitor.size().width as i32 - collapsed_width as i32 - margin;
    let physical_side = if (anchor.x - left_x).abs() <= (anchor.x - right_x).abs() {
        "left"
    } else {
        "right"
    };
    Some((
        SavedPosition {
            x: if physical_side == "left" {
                left_x
            } else {
                right_x
            },
            y: clamp_vertical(
                anchor.y,
                (FLOATING_ORB_SIZE * scale).round() as u32,
                monitor.position().y,
                monitor.size().height,
                margin,
            ),
        },
        physical_side,
    ))
}

#[cfg(target_os = "windows")]
#[tauri::command]
fn start_floating_orb_drag(
    app: AppHandle,
    cursor_start_x: i32,
    cursor_start_y: i32,
) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::POINT,
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON},
            WindowsAndMessaging::{
                GetCursorPos, SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
            },
        },
    };

    let window = app.get_webview_window("floating").ok_or("悬浮窗尚未创建")?;
    let state = app.state::<AppState>();
    let (direction, saved_anchor) = {
        let data = state
            .data
            .lock()
            .map_err(|_| "本地状态暂时不可用".to_owned())?;
        (
            data.floating_orb_expand_direction.clone(),
            data.floating_orb_position,
        )
    };
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let current = SavedPosition {
        x: position.x,
        y: position.y,
    };
    let anchor = orb_anchor_from_window(&window, &state, current)
        .or(saved_anchor)
        .ok_or("无法计算悬浮球位置")?;
    if state.floating_orb_dragging.swap(true, Ordering::Relaxed) {
        return Ok(());
    }
    let cursor_start = POINT {
        x: cursor_start_x,
        y: cursor_start_y,
    };
    if let Err(error) = set_orb_window_region(&window, false, "left") {
        state.floating_orb_dragging.store(false, Ordering::Relaxed);
        return Err(error);
    }
    if let Err(error) = resize_orb_window_from_anchor(&window, false, &direction, anchor) {
        state.floating_orb_dragging.store(false, Ordering::Relaxed);
        return Err(error);
    }
    let window_start = match window.outer_position() {
        Ok(position) => position,
        Err(error) => {
            state.floating_orb_dragging.store(false, Ordering::Relaxed);
            return Err(error.to_string());
        }
    };
    let hwnd = match window.hwnd() {
        Ok(hwnd) => hwnd.0 as isize,
        Err(error) => {
            state.floating_orb_dragging.store(false, Ordering::Relaxed);
            return Err(error.to_string());
        }
    };

    thread::spawn(move || {
        let mut moved = false;
        loop {
            let mut cursor = POINT { x: 0, y: 0 };
            if unsafe { GetCursorPos(&mut cursor) } != 0 {
                let delta_x = cursor.x - cursor_start.x;
                let delta_y = cursor.y - cursor_start.y;
                moved |= delta_x.abs() > 1 || delta_y.abs() > 1;
                unsafe {
                    SetWindowPos(
                        hwnd as _,
                        std::ptr::null_mut(),
                        window_start.x + delta_x,
                        window_start.y + delta_y,
                        0,
                        0,
                        SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
                    );
                }
            }
            if unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } >= 0 {
                break;
            }
            thread::sleep(Duration::from_millis(8));
        }
        let state = app.state::<AppState>();
        let mut at_edge = false;
        let mut side = "right".to_owned();
        if let Ok(position) = window.outer_position() {
            let mut final_anchor = SavedPosition {
                x: position.x,
                y: position.y,
            };
            if let Ok(monitor) = monitor_for_orb_anchor(&window, final_anchor) {
                let scale = monitor.scale_factor();
                let margin = (6.0 * scale).round() as i32;
                let size = (FLOATING_ORB_SIZE * scale).round() as i32;
                final_anchor.x = final_anchor.x.clamp(
                    monitor.position().x + margin,
                    monitor.position().x + monitor.size().width as i32 - size - margin,
                );
                final_anchor.y = clamp_vertical(
                    final_anchor.y,
                    size as u32,
                    monitor.position().y,
                    monitor.size().height,
                    margin,
                );
                if final_anchor.x != position.x || final_anchor.y != position.y {
                    let _ =
                        window.set_position(PhysicalPosition::new(final_anchor.x, final_anchor.y));
                }
            }
            at_edge = orb_is_near_edge(&window, &state, final_anchor).unwrap_or(false);
            if at_edge {
                if let Some((snapped, _)) = orb_edge_anchor(&window, &state, final_anchor) {
                    final_anchor = snapped;
                    let _ = resize_orb_window_from_anchor(&window, false, &direction, final_anchor);
                }
            }
            side = orb_layout_side_for_anchor(&window, &direction, final_anchor)
                .unwrap_or("right")
                .to_owned();
            let _ = remember_floating_position(&state, final_anchor);
        }
        state.floating_orb_dragging.store(false, Ordering::Relaxed);
        let _ = app.emit(
            "floating-orb-drag-ended",
            OrbDragResult {
                moved,
                at_edge,
                side,
            },
        );
    });

    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrbDragResult {
    moved: bool,
    at_edge: bool,
    side: String,
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn start_floating_orb_drag(
    app: AppHandle,
    _cursor_start_x: i32,
    _cursor_start_y: i32,
) -> Result<(), String> {
    let window = app.get_webview_window("floating").ok_or("悬浮窗尚未创建")?;
    window.start_dragging().map_err(|error| error.to_string())
}

fn orb_is_near_edge(
    window: &WebviewWindow,
    state: &AppState,
    position: SavedPosition,
) -> Result<bool, String> {
    let anchor = orb_anchor_from_window(window, state, position).ok_or("无法读取悬浮球位置")?;
    orb_anchor_is_near_edge(window, anchor)
}

fn orb_anchor_is_near_edge(window: &WebviewWindow, anchor: SavedPosition) -> Result<bool, String> {
    let monitor = monitor_for_orb_anchor(window, anchor)?;
    let scale = monitor.scale_factor();
    let margin = (6.0 * scale).round() as i32;
    let collapsed_width = (FLOATING_ORB_SIZE * scale).round() as u32;
    let left_x = monitor.position().x + margin;
    let right_x =
        monitor.position().x + monitor.size().width as i32 - collapsed_width as i32 - margin;
    let threshold = (28.0 * scale).round() as i32;
    Ok((anchor.x - left_x).abs() <= threshold || (anchor.x - right_x).abs() <= threshold)
}

#[cfg(target_os = "windows")]
fn set_orb_window_region(
    window: &WebviewWindow,
    collapsed: bool,
    side: &str,
) -> Result<(), String> {
    use windows_sys::Win32::Graphics::Gdi::{CreateEllipticRgn, DeleteObject, SetWindowRgn};

    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    if !collapsed {
        if unsafe { SetWindowRgn(hwnd.0 as _, std::ptr::null_mut(), 0) } == 0 {
            return Err("无法恢复悬浮球窗口区域".to_owned());
        }
        return Ok(());
    }
    let size = window.outer_size().map_err(|error| error.to_string())?;
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    let orb_size = (FLOATING_ORB_SIZE * scale).round() as i32;
    let left = if side == "right" {
        size.width as i32 - orb_size
    } else {
        0
    };
    let region = unsafe { CreateEllipticRgn(left, 0, left + orb_size, size.height as i32) };
    if region.is_null() {
        return Err("无法创建悬浮球交互区域".to_owned());
    }
    if unsafe { SetWindowRgn(hwnd.0 as _, region, 0) } == 0 {
        unsafe { DeleteObject(region as _) };
        return Err("无法设置悬浮球交互区域".to_owned());
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn set_orb_window_region(
    _window: &WebviewWindow,
    _collapsed: bool,
    _side: &str,
) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_orb_window_bounds(
    window: &WebviewWindow,
    logical_size: LogicalSize<f64>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};

    if window.outer_position().ok() == Some(PhysicalPosition::new(x, y))
        && window
            .outer_size()
            .ok()
            .is_some_and(|size| size.width == width && size.height == height)
    {
        return Ok(());
    }

    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    let result = unsafe {
        SetWindowPos(
            hwnd.0 as _,
            std::ptr::null_mut(),
            x,
            y,
            width as i32,
            height as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    };
    if result == 0 {
        return Err("无法同步悬浮球窗口位置和尺寸".to_owned());
    }
    let _ = logical_size;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn set_orb_window_bounds(
    window: &WebviewWindow,
    logical_size: LogicalSize<f64>,
    x: i32,
    y: i32,
    _width: u32,
    _height: u32,
) -> Result<(), String> {
    window
        .set_size(logical_size)
        .map_err(|error| error.to_string())?;
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())
}

fn resize_orb_window_from_side(
    window: &WebviewWindow,
    expanded: bool,
    direction: &str,
    current_side: Option<&str>,
) -> Result<String, String> {
    let monitor = window
        .current_monitor()
        .map_err(|error| error.to_string())?
        .or(window
            .primary_monitor()
            .map_err(|error| error.to_string())?)
        .ok_or("找不到当前显示器")?;
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let old_size = window.outer_size().map_err(|error| error.to_string())?;
    let side = orb_layout_side(window, direction)?;
    let current_side = current_side.unwrap_or(side);
    let logical = floating_logical_size("orb", expanded);
    let scale = monitor.scale_factor();
    let width = (logical.width * scale).round() as u32;
    let height = (logical.height * scale).round() as u32;
    let margin = (6.0 * scale).round() as i32;
    let orb_width = (56.0 * scale).round() as i32;
    let orb_left = if current_side == "right" {
        position.x + old_size.width as i32 - margin - orb_width
    } else {
        position.x + margin
    };

    // Keep the orb's physical position fixed. Only the transparent window
    // boundary moves around it while the teaser panel opens or closes.
    let x = orb_window_x(orb_left, orb_width, margin, side, width);
    let y = clamp_vertical(
        position.y,
        height,
        monitor.position().y,
        monitor.size().height,
        margin,
    );
    set_orb_window_bounds(window, logical, x, y, width, height)?;
    Ok(side.to_owned())
}

fn resize_orb_window_from_anchor(
    window: &WebviewWindow,
    expanded: bool,
    direction: &str,
    anchor: SavedPosition,
) -> Result<String, String> {
    let monitor = monitor_for_orb_anchor(window, anchor)?;
    let side = orb_layout_side_for_anchor(window, direction, anchor)?;
    let logical = floating_logical_size("orb", expanded);
    let scale = monitor.scale_factor();
    let width = (logical.width * scale).round() as u32;
    let height = (logical.height * scale).round() as u32;
    let margin = (6.0 * scale).round() as i32;
    let orb_width = (56.0 * scale).round() as i32;
    let orb_left = anchor.x + margin;
    let x = orb_window_x(orb_left, orb_width, margin, side, width);
    let y = clamp_vertical(
        anchor.y,
        height,
        monitor.position().y,
        monitor.size().height,
        margin,
    );
    set_orb_window_bounds(window, logical, x, y, width, height)?;
    Ok(side.to_owned())
}

fn apply_orb_display(
    window: &WebviewWindow,
    direction: &str,
    anchor: SavedPosition,
    panel_visible: bool,
) -> Result<String, String> {
    let side = resize_orb_window_from_anchor(window, true, direction, anchor)?;
    let clipped_to_orb = orb_anchor_is_near_edge(window, anchor)? && !panel_visible;
    set_orb_window_region(window, clipped_to_orb, &side)?;
    Ok(side)
}

fn snap_orb_to_edge(window: &WebviewWindow) -> Result<String, String> {
    let monitor = window
        .current_monitor()
        .map_err(|error| error.to_string())?
        .or(window
            .primary_monitor()
            .map_err(|error| error.to_string())?)
        .ok_or("找不到当前显示器")?;
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let size = window.outer_size().map_err(|error| error.to_string())?;
    let side = edge_side(
        position.x,
        size.width,
        monitor.position().x,
        monitor.size().width,
    );
    let margin = (6.0 * monitor.scale_factor()).round() as i32;
    let x = if side == "left" {
        monitor.position().x + margin
    } else {
        monitor.position().x + monitor.size().width as i32 - size.width as i32 - margin
    };
    let y = clamp_vertical(
        position.y,
        size.height,
        monitor.position().y,
        monitor.size().height,
        margin,
    );
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())?;
    Ok(side.to_owned())
}

#[tauri::command]
fn get_floating_orb_side(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let window = app.get_webview_window("floating").ok_or("悬浮窗尚未创建")?;
    let (direction, anchor) = {
        let data = state
            .data
            .lock()
            .map_err(|_| "本地状态暂时不可用".to_owned())?;
        (
            data.floating_orb_expand_direction.clone(),
            data.floating_orb_position,
        )
    };
    if let Some(anchor) = anchor {
        return Ok(orb_layout_side_for_anchor(&window, &direction, anchor)?.to_owned());
    }
    Ok(orb_layout_side(&window, &direction)?.to_owned())
}

#[tauri::command]
fn get_floating_orb_edge_state(app: AppHandle, state: State<'_, AppState>) -> Result<bool, String> {
    let window = app.get_webview_window("floating").ok_or("悬浮窗尚未创建")?;
    let position = window.outer_position().map_err(|error| error.to_string())?;
    orb_is_near_edge(
        &window,
        &state,
        SavedPosition {
            x: position.x,
            y: position.y,
        },
    )
}

#[cfg(target_os = "windows")]
#[tauri::command]
fn get_floating_orb_pointer_inside(app: AppHandle) -> Result<bool, String> {
    use windows_sys::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
    let window = app.get_webview_window("floating").ok_or("悬浮窗尚未创建")?;
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let size = window.outer_size().map_err(|error| error.to_string())?;
    let mut cursor = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut cursor) } == 0 {
        return Err("无法读取鼠标位置".to_owned());
    }
    Ok(cursor.x >= position.x
        && cursor.x < position.x + size.width as i32
        && cursor.y >= position.y
        && cursor.y < position.y + size.height as i32)
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn get_floating_orb_pointer_inside() -> bool {
    false
}

fn saved_floating_position(data: &PersistedState, style: &str) -> Option<SavedPosition> {
    match style {
        "orb" => data.floating_orb_position,
        _ => data.floating_card_position.or(data.floating_position),
    }
}

fn remember_floating_position(state: &AppState, position: SavedPosition) -> Result<(), String> {
    update_state(state, |data| match data.floating_style.as_str() {
        "orb" => data.floating_orb_position = Some(position),
        _ => data.floating_card_position = Some(position),
    })
}

#[tauri::command]
fn set_floating_style(
    app: AppHandle,
    state: State<'_, AppState>,
    style: String,
) -> Result<FloatingSettings, String> {
    if style != "card" && style != "orb" {
        return Err("悬浮窗样式无效".to_owned());
    }
    let window = app.get_webview_window("floating").ok_or("悬浮窗尚未创建")?;
    let current_style = state
        .data
        .lock()
        .map_err(|_| "本地状态暂时不可用".to_owned())?
        .floating_style
        .clone();
    if current_style != style {
        if let Ok(position) = window.outer_position() {
            let saved = SavedPosition {
                x: position.x,
                y: position.y,
            };
            let remembered = if current_style == "orb" {
                orb_anchor_from_window(&window, &state, saved).unwrap_or(saved)
            } else {
                saved
            };
            remember_floating_position(&state, remembered)?;
        }
    }
    state.floating_orb.store(style == "orb", Ordering::Relaxed);
    state.floating_orb_expanded.store(false, Ordering::Relaxed);
    set_orb_window_region(&window, false, "left")?;
    update_state(&state, |data| data.floating_style = style.clone())?;
    window
        .set_size(floating_logical_size(&style, false))
        .map_err(|error| error.to_string())?;
    let target_position = state
        .data
        .lock()
        .map_err(|_| "本地状态暂时不可用".to_owned())?
        .clone();
    if let Some(position) = saved_floating_position(&target_position, &style) {
        window
            .set_position(PhysicalPosition::new(position.x, position.y))
            .map_err(|error| error.to_string())?;
    } else if style == "orb" {
        let _ = snap_orb_to_edge(&window);
    } else {
        position_floating(&app);
    }
    let settings = get_floating_settings(state)?;
    let _ = app.emit("floating-settings-changed", settings.clone());
    Ok(settings)
}

#[tauri::command]
fn set_floating_orb_expanded(
    app: AppHandle,
    state: State<'_, AppState>,
    expanded: bool,
) -> Result<String, String> {
    let window = app.get_webview_window("floating").ok_or("悬浮窗尚未创建")?;
    let (direction, anchor) = {
        let data = state
            .data
            .lock()
            .map_err(|_| "本地状态暂时不可用".to_owned())?;
        (
            data.floating_orb_expand_direction.clone(),
            data.floating_orb_position,
        )
    };
    let anchor = if let Some(anchor) = anchor {
        anchor
    } else {
        let position = window.outer_position().map_err(|error| error.to_string())?;
        orb_anchor_from_window(
            &window,
            &state,
            SavedPosition {
                x: position.x,
                y: position.y,
            },
        )
        .ok_or("无法计算悬浮球位置")?
    };
    let side = apply_orb_display(&window, &direction, anchor, expanded)?;
    state
        .floating_orb_expanded
        .store(expanded, Ordering::Relaxed);
    Ok(side)
}

#[tauri::command]
fn set_floating_orb_expand_direction(
    app: AppHandle,
    state: State<'_, AppState>,
    direction: String,
) -> Result<FloatingSettings, String> {
    let direction = normalize_orb_expand_direction(&direction)
        .ok_or_else(|| "小球展开方向无效".to_owned())?
        .to_owned();
    let (previous_direction, anchor) = {
        let data = state
            .data
            .lock()
            .map_err(|_| "本地状态暂时不可用".to_owned())?;
        (
            data.floating_orb_expand_direction.clone(),
            data.floating_orb_position,
        )
    };
    update_state(&state, |data| {
        data.floating_orb_expand_direction = direction.clone()
    })?;

    if let Some(window) = app.get_webview_window("floating") {
        let is_orb = state
            .data
            .lock()
            .map_err(|_| "本地状态暂时不可用".to_owned())?
            .floating_style
            == "orb";
        if is_orb {
            let expanded = state.floating_orb_expanded.load(Ordering::Relaxed);
            if let Some(anchor) = anchor {
                let _ = apply_orb_display(&window, &direction, anchor, expanded);
            } else {
                let current_side = if expanded {
                    orb_layout_side(&window, &previous_direction).ok()
                } else {
                    None
                };
                let _ = resize_orb_window_from_side(&window, true, &direction, current_side);
            }
        }
    }

    let settings = get_floating_settings(state)?;
    let _ = app.emit("floating-settings-changed", settings.clone());
    Ok(settings)
}

#[tauri::command]
fn snap_floating_to_edge(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let window = app.get_webview_window("floating").ok_or("悬浮窗尚未创建")?;
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let (anchor, _) = orb_edge_anchor(
        &window,
        &state,
        SavedPosition {
            x: position.x,
            y: position.y,
        },
    )
    .ok_or("无法计算悬浮球吸附位置")?;
    let direction = state
        .data
        .lock()
        .map_err(|_| "本地状态暂时不可用".to_owned())?
        .floating_orb_expand_direction
        .clone();
    let side = apply_orb_display(&window, &direction, anchor, false)?;
    state.floating_orb_expanded.store(false, Ordering::Relaxed);
    remember_floating_position(&state, anchor)?;
    Ok(side)
}

#[tauri::command]
fn show_main_window_command(app: AppHandle) {
    show_main_window(&app);
}

#[tauri::command]
fn set_floating_pinned(
    app: AppHandle,
    state: State<'_, AppState>,
    pinned: bool,
) -> Result<bool, String> {
    state.floating_pinned.store(pinned, Ordering::Relaxed);
    update_state(&state, |data| data.floating_pinned = pinned)?;
    let _ = app.emit("floating-settings-changed", get_floating_settings(state)?);
    Ok(pinned)
}

#[tauri::command]
fn set_floating_opacity(
    app: AppHandle,
    state: State<'_, AppState>,
    opacity: f64,
) -> Result<f64, String> {
    let opacity = opacity.clamp(0.0, 1.0);
    update_state(&state, |data| data.floating_opacity = opacity)?;
    let _ = app.emit("floating-settings-changed", get_floating_settings(state)?);
    Ok(opacity)
}

#[tauri::command]
fn set_floating_always_on_top(
    app: AppHandle,
    state: State<'_, AppState>,
    _always_on_top: bool,
) -> Result<bool, String> {
    app.get_webview_window("floating")
        .ok_or("悬浮窗尚未创建")?
        .set_always_on_top(true)
        .map_err(|error| error.to_string())?;
    update_state(&state, |data| data.floating_always_on_top = true)?;
    let _ = app.emit("floating-settings-changed", get_floating_settings(state)?);
    Ok(true)
}

#[tauri::command]
fn set_display_mode(
    app: AppHandle,
    state: State<'_, AppState>,
    mode: String,
) -> Result<String, String> {
    if mode != "available" && mode != "used" {
        return Err("显示形式无效".to_owned());
    }
    update_state(&state, |data| data.display_mode = mode.clone())?;
    let _ = app.emit("floating-settings-changed", get_floating_settings(state)?);
    Ok(mode)
}

#[tauri::command]
fn set_theme(app: AppHandle, state: State<'_, AppState>, theme: String) -> Result<String, String> {
    if !matches!(
        theme.as_str(),
        "lime" | "cyan" | "violet" | "amber" | "rose"
    ) {
        return Err("主题配色无效".to_owned());
    }
    update_state(&state, |data| data.theme = theme.clone())?;
    let _ = app.emit("floating-settings-changed", get_floating_settings(state)?);
    Ok(theme)
}

#[tauri::command]
fn set_proxy_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    mode: String,
    address: String,
) -> Result<FloatingSettings, String> {
    if !matches!(mode.as_str(), "system" | "none" | "custom") {
        return Err("代理模式无效".to_owned());
    }
    let address = if mode == "custom" {
        normalize_proxy_address(&address)?
    } else {
        address.trim().to_owned()
    };
    update_state(&state, |data| {
        data.proxy_mode = mode;
        data.proxy_address = address;
    })?;
    let settings = get_floating_settings(state)?;
    let _ = app.emit("floating-settings-changed", settings.clone());
    Ok(settings)
}

fn toggle_pin(app: &AppHandle) -> Result<bool, String> {
    let next = !app
        .state::<AppState>()
        .data
        .lock()
        .map_err(|_| "本地状态暂时不可用".to_owned())?
        .floating_pinned;
    set_floating_pinned(app.clone(), app.state::<AppState>(), next)
}

fn position_floating(app: &AppHandle) {
    let Some(window) = app.get_webview_window("floating") else {
        return;
    };
    let Ok(Some(monitor)) = window.primary_monitor() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let margin = (18.0 * monitor.scale_factor()).round() as i32;
    let x = monitor.position().x + monitor.size().width as i32 - size.width as i32 - margin;
    let y = monitor.position().y + margin;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

fn position_intersects_monitor(
    position: SavedPosition,
    window_width: u32,
    window_height: u32,
    monitor_x: i32,
    monitor_y: i32,
    monitor_width: u32,
    monitor_height: u32,
) -> bool {
    let left = position.x.max(monitor_x);
    let top = position.y.max(monitor_y);
    let right = (position.x + window_width as i32).min(monitor_x + monitor_width as i32);
    let bottom = (position.y + window_height as i32).min(monitor_y + monitor_height as i32);
    let minimum_visible_width = (window_width.min(180) as i32).max(80);
    let minimum_visible_height = (window_height.min(140) as i32).max(70);
    right - left >= minimum_visible_width && bottom - top >= minimum_visible_height
}

fn position_is_visible(window: &WebviewWindow, position: SavedPosition) -> bool {
    let Ok(size) = window.outer_size() else {
        return false;
    };
    let Ok(monitors) = window.available_monitors() else {
        return false;
    };
    monitors.into_iter().any(|monitor| {
        let monitor_position = monitor.position();
        let monitor_size = monitor.size();
        position_intersects_monitor(
            position,
            size.width,
            size.height,
            monitor_position.x,
            monitor_position.y,
            monitor_size.width,
            monitor_size.height,
        )
    })
}

fn center_main_window(window: &WebviewWindow) {
    let Ok(Some(monitor)) = window.primary_monitor() else {
        let _ = window.center();
        return;
    };
    let Ok(size) = window.outer_size() else {
        let _ = window.center();
        return;
    };
    let monitor_position = monitor.position();
    let monitor_size = monitor.size();
    let x = monitor_position.x + ((monitor_size.width as i32 - size.width as i32) / 2).max(0);
    let y = monitor_position.y + ((monitor_size.height as i32 - size.height as i32) / 2).max(0);
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

fn restore_windows(app: &AppHandle, data: &PersistedState) {
    if let Some(window) = app.get_webview_window("main") {
        if let Some(position) = data
            .main_position
            .filter(|position| position_is_visible(&window, *position))
        {
            let _ = window.set_position(PhysicalPosition::new(position.x, position.y));
        } else {
            center_main_window(&window);
        }
    }
    if let Some(window) = app.get_webview_window("floating") {
        let _ = window.set_size(floating_logical_size(&data.floating_style, false));
        let saved_position = saved_floating_position(data, &data.floating_style);
        if let Some(position) = saved_position {
            let _ = window.set_position(PhysicalPosition::new(position.x, position.y));
        } else if data.floating_style == "orb" {
            let _ = snap_orb_to_edge(&window);
        } else {
            position_floating(app);
        }
        let _ = window.set_always_on_top(true);
        if data.floating_visible {
            let _ = window.show();
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "windows")]
    windows_integration::initialize_taskbar();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            handle_launch_action(app, &args);
        }))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = executable_data_dir().map_err(std::io::Error::other)?;
            fs::create_dir_all(&data_dir)?;
            let file_path = data_dir.join("state.json");
            let legacy_file = app.path().app_data_dir()?.join("data/state.json");
            migrate_legacy_state(&legacy_file, &file_path);
            let mut persisted = load_state(&file_path);
            let repair_topmost_setting = !persisted.floating_always_on_top;
            persisted.floating_always_on_top = true;
            let save_sender = start_state_writer(file_path.clone());
            let floating_pinned = Arc::new(AtomicBool::new(persisted.floating_pinned));
            let floating_orb = Arc::new(AtomicBool::new(persisted.floating_style == "orb"));
            app.manage(AppState {
                file_path,
                data: Mutex::new(persisted.clone()),
                save_sender,
                floating_pinned: Arc::clone(&floating_pinned),
                floating_orb: Arc::clone(&floating_orb),
                floating_orb_dragging: AtomicBool::new(false),
                floating_orb_expanded: AtomicBool::new(false),
            });
            if repair_topmost_setting {
                let _ = update_state(&app.state::<AppState>(), |data| {
                    data.floating_always_on_top = true;
                });
            }
            restore_windows(app.handle(), &persisted);
            if let Some(window) = app.get_webview_window("floating") {
                start_click_through_controller(window, floating_pinned, floating_orb);
            }
            let show = MenuItem::with_id(app, "show-main", "显示主窗口", true, None::<&str>)?;
            let floating = MenuItem::with_id(
                app,
                "toggle-floating",
                "显示 / 隐藏悬浮窗",
                true,
                None::<&str>,
            )?;
            let pin = MenuItem::with_id(
                app,
                "toggle-pin",
                "固定 / 取消固定悬浮窗",
                true,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "完全退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &floating, &pin, &quit])?;
            TrayIconBuilder::with_id("codex-quota-tray")
                .icon(app.default_window_icon().expect("default app icon").clone())
                .tooltip("Codex 额度")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show-main" => show_main_window(app),
                    "toggle-floating" => {
                        let _ = toggle_floating(app);
                    }
                    "toggle-pin" => {
                        let _ = toggle_pin(app);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;
            #[cfg(target_os = "windows")]
            windows_integration::refresh_taskbar();
            let args = env::args().collect::<Vec<_>>();
            handle_launch_action(app.handle(), &args);
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::Moved(position) => {
                let saved = SavedPosition {
                    x: position.x,
                    y: position.y,
                };
                let label = window.label().to_owned();
                let state = window.app_handle().state::<AppState>();
                if label == "main" {
                    let _ = update_state(&state, |data| data.main_position = Some(saved));
                } else if label == "floating" {
                    if state.floating_orb.load(Ordering::Relaxed) {
                        return;
                    }
                    if state.floating_orb_dragging.load(Ordering::Relaxed) {
                        return;
                    }
                    let orb_position = {
                        let is_orb = state
                            .data
                            .lock()
                            .map(|data| data.floating_style == "orb")
                            .unwrap_or(false);
                        if is_orb {
                            window
                                .app_handle()
                                .get_webview_window("floating")
                                .and_then(|floating| {
                                    orb_anchor_from_window(&floating, &state, saved)
                                })
                                .unwrap_or(saved)
                        } else {
                            saved
                        }
                    };
                    let _ = remember_floating_position(&state, orb_position);
                }
            }
            WindowEvent::Resized(_)
                if window.label() == "main" && window.is_minimized().unwrap_or(false) =>
            {
                let _ = window.hide();
            }
            WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                api.prevent_close();
                window.app_handle().exit(0);
            }
            WindowEvent::CloseRequested { api, .. } if window.label() == "floating" => {
                api.prevent_close();
                let _ = set_floating_visible(window.app_handle(), false);
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_codex_usage,
            get_cached_usage,
            set_floating_window,
            get_floating_settings,
            set_floating_pinned,
            set_floating_opacity,
            set_floating_always_on_top,
            set_floating_style,
            set_floating_orb_expanded,
            set_floating_orb_expand_direction,
            get_floating_orb_side,
            get_floating_orb_edge_state,
            get_floating_orb_pointer_inside,
            snap_floating_to_edge,
            start_floating_orb_drag,
            show_main_window_command,
            set_display_mode,
            set_theme,
            set_proxy_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_vertical, edge_side, find_codex_launcher, is_floating_pin_hit, launch_action,
        normalize_orb_expand_direction, normalize_proxy_address, orb_anchor_x_from_window,
        orb_side_for_direction, orb_window_x, parse_usage_responses, position_intersects_monitor,
        read_codex_usage, LaunchAction, NetworkSettings, PersistedState, SavedPosition,
    };
    use serde_json::json;

    #[test]
    fn parses_windows_jump_list_actions() {
        assert_eq!(
            launch_action(&["codex-quota.exe".into(), "--toggle-floating".into()]),
            LaunchAction::ToggleFloating
        );
        assert_eq!(
            launch_action(&["codex-quota.exe".into(), "--toggle-pin".into()]),
            LaunchAction::TogglePin
        );
        assert_eq!(
            launch_action(&["codex-quota.exe".into(), "--quit".into()]),
            LaunchAction::Quit
        );
        assert_eq!(
            launch_action(&["codex-quota.exe".into()]),
            LaunchAction::ShowMain
        );
    }

    #[test]
    fn reports_a_clear_error_when_codex_is_not_logged_in() {
        let rate = json!({"id":2,"error":{"message":"authentication required"}});
        let account = json!({"id":3,"result":{"account":null,"requiresOpenaiAuth":true}});
        let error = parse_usage_responses(&rate, Some(&account), 0).unwrap_err();
        assert!(error.contains("尚未登录") && error.contains("codex login"));
    }

    #[test]
    fn preserves_server_reset_timestamp_without_recalculating_it() {
        let rate = json!({"id":2,"result":{"rateLimits":{"primary":{"usedPercent":12,"windowDurationMins":300,"resetsAt":1790056965},"secondary":null}}});
        let account =
            json!({"id":3,"result":{"account":{"type":"chatgpt","email":"test@example.com"}}});
        assert_eq!(
            parse_usage_responses(&rate, Some(&account), 100)
                .unwrap()
                .primary
                .unwrap()
                .resets_at,
            1790056965
        );
    }

    #[test]
    fn persisted_state_is_backward_compatible() {
        let state: PersistedState = serde_json::from_str("{}").unwrap();
        assert_eq!(state.floating_opacity, 0.92);
        assert!(!state.floating_pinned);
        assert!(state.floating_always_on_top);
        assert_eq!(state.floating_style, "card");
        assert!(state.floating_card_position.is_none());
        assert!(state.floating_orb_position.is_none());
        assert_eq!(state.floating_orb_expand_direction, "auto");
        assert_eq!(state.display_mode, "available");
        assert_eq!(state.proxy_mode, "system");
    }

    #[test]
    fn keeps_card_and_orb_positions_in_separate_slots() {
        let state: PersistedState = serde_json::from_value(json!({
            "floatingCardPosition": {"x": 10, "y": 20},
            "floatingOrbPosition": {"x": 1800, "y": 400}
        }))
        .unwrap();
        assert_eq!(state.floating_card_position.unwrap().x, 10);
        assert_eq!(state.floating_orb_position.unwrap().x, 1800);
    }

    #[test]
    fn chooses_the_nearest_monitor_edge_for_the_orb() {
        assert_eq!(edge_side(30, 52, 0, 1920), "left");
        assert_eq!(edge_side(1800, 52, 0, 1920), "right");
        assert_eq!(edge_side(-1880, 52, -1920, 1920), "left");
    }

    #[test]
    fn accepts_only_supported_orb_expand_directions() {
        assert_eq!(normalize_orb_expand_direction("auto"), Some("auto"));
        assert_eq!(normalize_orb_expand_direction("left"), Some("left"));
        assert_eq!(normalize_orb_expand_direction("right"), Some("right"));
        assert_eq!(normalize_orb_expand_direction("bottom"), None);
        assert_eq!(orb_side_for_direction("left", "left"), "right");
        assert_eq!(orb_side_for_direction("right", "right"), "left");
        assert_eq!(orb_side_for_direction("auto", "right"), "right");
    }

    #[test]
    fn keeps_the_orb_anchor_when_window_expands_or_collapses() {
        let orb_left = 506;
        let orb_width = 56;
        let margin = 6;
        assert_eq!(orb_window_x(orb_left, orb_width, margin, "left", 68), 500);
        assert_eq!(orb_window_x(orb_left, orb_width, margin, "right", 230), 338);
        assert_eq!(orb_window_x(orb_left, orb_width, margin, "right", 68), 500);
        assert_eq!(orb_anchor_x_from_window(500, 68, 68, "left"), 500);
        assert_eq!(orb_anchor_x_from_window(338, 230, 68, "right"), 500);
        assert_eq!(orb_anchor_x_from_window(500, 68, 68, "right"), 500);
    }

    #[test]
    fn keeps_the_orb_inside_the_monitor_vertical_bounds() {
        assert_eq!(clamp_vertical(-50, 52, 0, 1080, 6), 6);
        assert_eq!(clamp_vertical(500, 52, 0, 1080, 6), 500);
        assert_eq!(clamp_vertical(2000, 52, 0, 1080, 6), 1022);
    }

    #[test]
    fn rejects_saved_main_window_positions_outside_the_current_monitors() {
        assert!(!position_intersects_monitor(
            SavedPosition {
                x: -32000,
                y: -32000
            },
            960,
            680,
            0,
            0,
            1920,
            1080
        ));
        assert!(position_intersects_monitor(
            SavedPosition { x: 120, y: 80 },
            960,
            680,
            0,
            0,
            1920,
            1080
        ));
    }

    #[test]
    fn pinned_window_only_keeps_the_pin_button_interactive() {
        assert!(is_floating_pin_hit(225, 18, 0, 0, 308, 174));
        assert!(!is_floating_pin_hit(100, 80, 0, 0, 308, 174));
        assert!(!is_floating_pin_hit(270, 18, 0, 0, 308, 174));
    }

    #[test]
    fn normalizes_local_proxy_addresses() {
        assert_eq!(
            normalize_proxy_address("127.0.0.1:7890").unwrap(),
            "http://127.0.0.1:7890"
        );
        assert!(normalize_proxy_address("").is_err());
        assert!(normalize_proxy_address("ftp://127.0.0.1:21").is_err());
    }

    #[test]
    fn finds_an_installed_codex_binary() {
        assert!(!find_codex_launcher(None)
            .unwrap()
            .program
            .as_os_str()
            .is_empty());
    }

    #[test]
    #[ignore = "requires a live Codex account and network access"]
    fn reads_rate_limits_from_local_codex_session() {
        assert!(read_codex_usage(
            None,
            NetworkSettings {
                mode: "system".to_owned(),
                address: String::new(),
            }
        )
        .unwrap()
        .0
        .primary
        .is_some());
    }
}

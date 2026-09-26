// 用户可配置快捷键（主窗口「快捷键」页）。
//
// 七个动作各有绑定（默认一组，可录制替换）：
//   主窗口显示/隐藏 · 壁纸设置窗口显示/隐藏 · 手动暂停/播放 · 自动暂停开启/关闭 ·
//   定时切换开启/关闭 · 下一个壁纸 · 上一个壁纸（后两个仅定时切换开启时生效）。
//
// 注册策略（macOS 的关键取舍）：
//   - **⌘+单键**（⌘M/⌘H 这类系统惯例键）注册为**应用菜单快捷键**：全局注册会
//     把其它所有 App 的 ⌘M/⌘H 一起劫走（最小化/隐藏失效），所以只在本应用
//     聚焦时生效 —— 这正是「mac 下默认的 command+m/h」应有的语义。
//   - **其余组合**注册为全局热键（tauri-plugin-global-shortcut），游戏/其它
//     应用在前台时也能用（⌘⇧P 暂停这一档一直是全局的，保持不变）。
//
// 录制的坑（用户明确点过名）：录制时如果菜单和全局热键还在，⌘+任意键会被
// 菜单键位或全局热键**吞掉**，keydown 根本到不了录制框（「不能录制 command+
// 其它键」）。所以录制前后整组挂起/恢复：hotkeys_record_begin 摘菜单 + 注销
// 全部全局热键，record_end 反向恢复。
use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{
    menu::{Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu},
    AppHandle, Manager, Wry,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

/// 动作 id → 界面文案（中文原文即 i18n 键，同全项目惯例）。
/// 顺序 = 「快捷键」页的展示顺序。
pub const ACTIONS: &[(&str, &str)] = &[
    ("toggle_main_window", "主窗口显示/隐藏"),
    ("toggle_props_window", "壁纸设置窗口显示/隐藏"),
    ("toggle_pause", "手动暂停/播放"),
    ("toggle_auto_pause", "自动暂停开启/关闭"),
    ("toggle_rotation", "定时切换开启/关闭"),
    ("next_wallpaper", "下一个壁纸"),
    ("prev_wallpaper", "上一个壁纸"),
];

/// macOS 默认：主窗口用系统惯例键 ⌘M/⌘H（菜单快捷键，不劫持其它 App），
/// 另配一枚 ⌘⇧M 作**全局**键 —— ⌘M/⌘H 只在应用内生效，窗口隐藏后人在
/// 其它应用里要能把主窗口唤回来，必须有一枚全局的。
/// 其余沿用 ⌘⇧ 字母（历史上就在这套）。
#[cfg(target_os = "macos")]
fn default_bindings(action: &str) -> Vec<String> {
    match action {
        "toggle_main_window" => vec!["cmd+m".into(), "cmd+h".into(), "cmd+shift+m".into()],
        "toggle_props_window" => vec!["cmd+shift+s".into()],
        "toggle_pause" => vec!["cmd+shift+p".into()],
        "toggle_auto_pause" => vec!["cmd+shift+a".into()],
        "toggle_rotation" => vec!["cmd+shift+r".into()],
        "next_wallpaper" => vec!["cmd+shift+n".into()],
        "prev_wallpaper" => vec!["cmd+shift+b".into()],
        _ => vec![],
    }
}

/// 非 macOS：Ctrl+Shift+字母，全部走全局热键（无菜单键位可依）。
#[cfg(not(target_os = "macos"))]
fn default_bindings(action: &str) -> Vec<String> {
    match action {
        "toggle_main_window" => vec!["ctrl+shift+m".into()],
        "toggle_props_window" => vec!["ctrl+shift+s".into()],
        "toggle_pause" => vec!["ctrl+shift+p".into()],
        "toggle_auto_pause" => vec!["ctrl+shift+a".into()],
        "toggle_rotation" => vec!["ctrl+shift+r".into()],
        "next_wallpaper" => vec!["ctrl+shift+n".into()],
        "prev_wallpaper" => vec!["ctrl+shift+b".into()],
        _ => vec![],
    }
}

/// 系统/菜单已占用、不建议覆盖的组合（macOS）。⌘M/⌘H 不在其中 —— 它们正是
/// 「主窗口显示/隐藏」的默认值，属于我们自己的动作。
#[cfg(target_os = "macos")]
pub const RESERVED: &[&str] = &[
    "cmd+q", "cmd+w", "cmd+space", "cmd+tab", "cmd+`", "cmd+z", "cmd+shift+z", "cmd+x", "cmd+c",
    "cmd+v", "cmd+a",
];
#[cfg(not(target_os = "macos"))]
pub const RESERVED: &[&str] = &["ctrl+alt+delete", "ctrl+c", "ctrl+v", "ctrl+x", "ctrl+a"];

const SETTING_KEY: &str = "hotkeys_v1";

pub struct Hotkeys {
    /// 动作 id → 绑定列表（规范小写写法，如 "cmd+shift+p"）
    bindings: Mutex<HashMap<String, Vec<String>>>,
    /// 反查表：Shortcut 的 Display → 动作 id（handler 只拿得到 Shortcut；
    /// 两侧都经同一类型序列化当键，格式不依赖手写约定）
    lookup: Mutex<HashMap<String, String>>,
    /// 录制挂起前的应用菜单（hotkeys_record_end 时原样装回）
    saved_menu: Mutex<Option<Menu<Wry>>>,
    /// 录制中：全局热键与菜单已摘除
    suspended: Mutex<bool>,
}

impl Hotkeys {
    fn snapshot(&self) -> HashMap<String, Vec<String>> {
        self.bindings.lock().unwrap().clone()
    }
}

/// 绑定是否是「⌘+单键」：macOS 走菜单快捷键，其余走全局热键。
fn is_mac_menu_accel(accel: &str) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    let parts: Vec<&str> = accel.split('+').map(str::trim).collect();
    parts.len() == 2 && parts[0] == "cmd" && parts[1].len() == 1
}

/// "cmd+shift+s" → 菜单加速键写法 "Cmd+Shift+S"（muda 的 Accelerator 解析用）。
fn menu_accel(accel: &str) -> String {
    accel
        .split('+')
        .map(|p| match p {
            "cmd" => "Cmd".into(),
            "ctrl" => "Ctrl".into(),
            "alt" => "Alt".into(),
            "shift" => "Shift".into(),
            k if k.len() == 1 => k.to_uppercase(),
            // f1 / space / up 这类命名键：首字母大写即可（F1 / Space / Up）
            k => {
                let mut c = k.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 菜单项文案：⌘M/⌘H 用 macOS 惯例说法，其余用动作名。
fn menu_label(action: &str, accel: &str) -> &'static str {
    match accel {
        "cmd+m" => "最小化主窗口",
        "cmd+h" => "隐藏主窗口",
        _ => action_label(action),
    }
}

fn action_label(action: &str) -> &'static str {
    ACTIONS
        .iter()
        .find(|(id, _)| *id == action)
        .map(|(_, label)| *label)
        .unwrap_or("快捷键")
}

/// 校验一条绑定：至少一个修饰键（或独立 F 键），能被 Shortcut 解析。
pub fn validate_accel(accel: &str) -> Result<(), String> {
    let parts: Vec<&str> = accel.split('+').map(str::trim).collect();
    let (mods, key): (Vec<&str>, &str) = match parts.split_last() {
        Some((k, m)) => (m.to_vec(), *k),
        None => return Err("空快捷键".into()),
    };
    if key.is_empty() {
        return Err("缺少按键".into());
    }
    let has_mod = ["cmd", "ctrl", "alt", "shift"].iter().any(|m| mods.contains(m));
    let is_fn = key.len() > 1 && key.starts_with('f') && key[1..].parse::<u32>().is_ok();
    if !has_mod && !is_fn {
        return Err("至少需要一个修饰键（⌘/Ctrl/Alt/Shift）或使用 F 键".into());
    }
    accel
        .parse::<Shortcut>()
        .map(|_| ())
        .map_err(|e| format!("无法识别的快捷键 {accel}: {e}"))
}

/// 启动时读库 + 注册。DB 未就绪的键走默认值。
pub fn init(app: &AppHandle) -> Result<(), String> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    let stored = app
        .try_state::<std::sync::Arc<Mutex<rusqlite::Connection>>>()
        .and_then(|db| {
            db.lock()
                .ok()
                .and_then(|c| crate::db::get_setting(&c, SETTING_KEY))
        });
    if let Some(raw) = stored {
        if let Ok(parsed) = serde_json::from_str::<HashMap<String, Vec<String>>>(&raw) {
            for (k, v) in parsed {
                // 空列表是合法状态（用户清空了该动作的绑定），不能因为 is_empty 被丢掉
                let ok = ACTIONS.iter().any(|(id, _)| *id == k)
                    && v.iter().all(|a| validate_accel(a).is_ok());
                if ok {
                    map.insert(k, v);
                }
            }
        }
    }
    for (id, _) in ACTIONS {
        map.entry((*id).to_string())
            .or_insert_with(|| default_bindings(id));
    }
    app.manage(Hotkeys {
        bindings: Mutex::new(map),
        lookup: Mutex::new(HashMap::new()),
        saved_menu: Mutex::new(None),
        suspended: Mutex::new(false),
    });
    // 注册失败不能掀掉整个 setup：绑定已入库，下次启动或改绑时会再试
    if let Err(e) = apply(app) {
        tracing::warn!("hotkeys apply at init failed (bindings kept): {e}");
    }
    Ok(())
}

/// （重新）注册全部绑定：装菜单（⌘+单键）+ 注册全局热键（其余）。
pub fn apply(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Hotkeys>();
    let bindings = state.snapshot();
    *state.suspended.lock().unwrap() = false;

    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    // 快捷键 → 动作 的反查表（handler 只拿得到 Shortcut，用它的 Display 当键，
    // 两侧都经同一类型序列化，格式不依赖手写约定）
    let mut lookup: HashMap<String, String> = HashMap::new();

    #[cfg(target_os = "macos")]
    {
        let menu = build_app_menu(app, &bindings).map_err(|e| e.to_string())?;
        *state.saved_menu.lock().unwrap() = Some(menu.clone());
        app.set_menu(menu).map_err(|e| e.to_string())?;
    }

    let mut registered: Vec<String> = Vec::new();
    for (action, accels) in &bindings {
        for accel in accels {
            if is_mac_menu_accel(accel) {
                continue; // 菜单快捷键已在上面处理
            }
            match gs.register(accel.as_str()) {
                Ok(_) => {
                    registered.push(accel.clone());
                    if let Ok(sk) = accel.parse::<Shortcut>() {
                        lookup.insert(sk.to_string(), action.clone());
                    }
                }
                Err(e) => {
                    // 半途失败要收干净：注销已注册的，别留一堆孤儿热键
                    for a in &registered {
                        let _ = gs.unregister(a.as_str());
                    }
                    return Err(format!("快捷键 {accel} 注册失败（可能被系统或其它应用占用）: {e}"));
                }
            }
        }
    }
    // 反查表存回 state：给 handler 用
    *state.lookup.lock().unwrap() = lookup;
    Ok(())
}

/// 全局热键 handler 入口：Shortcut → 动作 id → 分发。
pub fn dispatch_by_shortcut(app: &AppHandle, shortcut: &Shortcut) {
    let key = shortcut.to_string();
    let action = app
        .try_state::<Hotkeys>()
        .and_then(|s| s.lookup.lock().unwrap().get(&key).cloned());
    if let Some(action) = action {
        dispatch(app, &action);
    } else {
        tracing::warn!("global shortcut without action: {key}");
    }
}

/// 录制开始：摘菜单 + 注销全局热键，让 ⌘+任意键都能到达录制框。
#[tauri::command]
pub fn hotkeys_record_begin(app: AppHandle) -> Result<(), String> {
    let state = app.state::<Hotkeys>();
    *state.suspended.lock().unwrap() = true;
    let _ = app.global_shortcut().unregister_all();
    #[cfg(target_os = "macos")]
    {
        if let Ok(Some(menu)) = app.remove_menu() {
            *state.saved_menu.lock().unwrap() = Some(menu);
        }
    }
    Ok(())
}

/// 录制结束（成功或取消都要调）：恢复菜单与全部热键。
#[tauri::command]
pub fn hotkeys_record_end(app: AppHandle) -> Result<(), String> {
    apply(&app)
}

/// 当前绑定 + 默认值（前端展示/恢复默认用）。
#[tauri::command]
pub fn hotkeys_list(app: AppHandle) -> Result<serde_json::Value, String> {
    let state = app.try_state::<Hotkeys>().ok_or("快捷键未初始化")?;
    let bindings = state.snapshot();
    let items: Vec<serde_json::Value> = ACTIONS
        .iter()
        .map(|(id, label)| {
            serde_json::json!({
                "id": id,
                "label": label,
                "bindings": bindings.get(*id).cloned().unwrap_or_default(),
                "defaults": default_bindings(id),
            })
        })
        .collect();
    Ok(serde_json::json!({
        "items": items,
        "reserved": RESERVED,
    }))
}

/// 设置某动作的绑定（录制结果落地）。整组替换；空列表 = 该动作暂不绑定。
/// `force` = 覆盖模式：注册失败（可能被其它应用占用）也照存不误，
/// 用于「被占用了…可以取消或者覆盖」里的覆盖。
#[tauri::command]
pub fn hotkeys_set(
    app: AppHandle,
    action: String,
    bindings: Vec<String>,
    force: Option<bool>,
) -> Result<(), String> {
    if !ACTIONS.iter().any(|(id, _)| *id == action) {
        return Err(format!("未知的快捷键动作: {action}"));
    }
    for a in &bindings {
        validate_accel(a)?;
    }
    let state = app.state::<Hotkeys>();
    // 互斥：同一组合不能挂两个动作
    {
        let map = state.bindings.lock().unwrap();
        for (other, accels) in map.iter() {
            if *other == action {
                continue;
            }
            if let Some(hit) = accels.iter().find(|a| bindings.contains(a)) {
                return Err(format!("快捷键 {hit} 已被「{}」占用", action_label(other)));
            }
        }
    }
    let old = {
        let mut map = state.bindings.lock().unwrap();
        map.insert(action.clone(), bindings.clone())
    };
    // 注册失败回滚旧绑定，别把用户的原快捷键弄丢；force = 覆盖：照存
    if let Err(e) = apply(&app) {
        if force != Some(true) {
            let mut map = state.bindings.lock().unwrap();
            match old {
                Some(prev) => {
                    map.insert(action.clone(), prev);
                }
                None => {
                    map.remove(&action);
                }
            }
            let _ = apply(&app);
            return Err(e);
        }
        tracing::warn!("hotkeys_set force-keep {action} despite register failure: {e}");
    }
    save(&app);
    Ok(())
}

/// 恢复某动作（或全部）的默认绑定。
#[tauri::command]
pub fn hotkeys_reset(app: AppHandle, action: Option<String>) -> Result<(), String> {
    let state = app.state::<Hotkeys>();
    {
        let mut map = state.bindings.lock().unwrap();
        match action.as_deref() {
            Some(id) => {
                if !ACTIONS.iter().any(|(aid, _)| *aid == id) {
                    return Err(format!("未知的快捷键动作: {id}"));
                }
                map.insert(id.to_string(), default_bindings(id));
            }
            None => {
                for (id, _) in ACTIONS {
                    map.insert((*id).to_string(), default_bindings(id));
                }
            }
        }
    }
    apply(&app)?;
    save(&app);
    Ok(())
}

fn save(app: &AppHandle) {
    let Some(state) = app.try_state::<Hotkeys>() else {
        return;
    };
    let bindings = state.snapshot();
    if let Some(db) = app.try_state::<std::sync::Arc<Mutex<rusqlite::Connection>>>() {
        if let Ok(conn) = db.lock() {
            if let Ok(json) = serde_json::to_string(&bindings) {
                let _ = crate::db::set_setting(&conn, SETTING_KEY, &json);
            }
        }
    }
}

/// 应用菜单（macOS）：app 子菜单 + 快捷键占位 + Edit（同 lib.rs 的裁剪策略）。
/// ⌘+单键绑定都落成菜单项 —— 这是 macOS 上 ⌘M/⌘H 这类组合唯一不劫持
/// 其它 App 的注册方式。
#[cfg(target_os = "macos")]
fn build_app_menu(
    app: &AppHandle,
    bindings: &HashMap<String, Vec<String>>,
) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
        let items = app_menu.items()?;
        // Menu::default 的 app 子菜单布局固定：
        // [About, sep, Services, sep, Hide, HideOthers, sep, Quit]
        if let Some(hide) = items.get(4) {
            let _ = app_menu.remove(hide);
        }
        let mut idx = 4usize;
        for (action, _) in ACTIONS {
            for accel in bindings.get(*action).into_iter().flatten() {
                if !is_mac_menu_accel(accel) {
                    continue;
                }
                let item = MenuItem::with_id(
                    app,
                    format!("hotkey::{action}::{accel}"),
                    crate::i18n::tr(menu_label(action, accel)),
                    true,
                    Some(menu_accel(accel).as_str()),
                )?;
                app_menu.insert(&item, idx)?;
                idx += 1;
            }
        }
    }
    // 同 lib.rs 的裁剪：只留 app 子菜单 + Edit（WKWebView 输入框要 ⌘C/⌘V/⌘A）
    let kinds = menu.items()?;
    for (i, kind) in kinds.into_iter().enumerate() {
        if i == 0 {
            continue;
        }
        let _ = menu.remove(&kind);
    }
    let undo = PredefinedMenuItem::undo(app, None)?;
    let redo = PredefinedMenuItem::redo(app, None)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let cut = PredefinedMenuItem::cut(app, None)?;
    let copy = PredefinedMenuItem::copy(app, None)?;
    let paste = PredefinedMenuItem::paste(app, None)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let select_all = PredefinedMenuItem::select_all(app, None)?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[&undo, &redo, &sep1, &cut, &copy, &paste, &sep2, &select_all],
    )?;
    menu.insert(&edit, 1usize)?;
    Ok(menu)
}

/// 菜单项点击（⌘+单键绑定）→ 分发。返回是否认领了这条事件。
#[cfg(target_os = "macos")]
pub fn handle_menu_event(app: &AppHandle, id: &str) -> bool {
    // id 形如 hotkey::<action>::<accel>
    let Some(rest) = id.strip_prefix("hotkey::") else {
        return false;
    };
    let Some(action) = rest.split("::").next() else {
        return false;
    };
    dispatch(app, action);
    true
}

#[cfg(not(target_os = "macos"))]
pub fn handle_menu_event(_app: &AppHandle, _id: &str) -> bool {
    false // 非 mac 无应用菜单
}

/// 语言切换后重装菜单（菜单文案要跟着变）。非 mac 空实现。
pub fn retranslate(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let Some(state) = app.try_state::<Hotkeys>() else {
            return;
        };
        if *state.suspended.lock().unwrap() {
            return; // 录制中菜单是摘掉的，结束时会重建
        }
        let bindings = state.snapshot();
        if let Ok(menu) = build_app_menu(app, &bindings) {
            *state.saved_menu.lock().unwrap() = Some(menu.clone());
            let _ = app.set_menu(menu);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }
}

// ---- 动作分发 ----

pub fn dispatch(app: &AppHandle, action: &str) {
    match action {
        "toggle_main_window" => {
            let visible = app
                .get_webview_window("main")
                .map(|w| w.is_visible().unwrap_or(false))
                .unwrap_or(false);
            if visible {
                // 与窗口关闭同语义：隐藏即释放（minimize 观察器会销毁窗口回收内存）
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.minimize();
                }
            } else {
                crate::main_window::ensure_main_window(app);
            }
        }
        "toggle_props_window" => {
            let open: Vec<_> = app
                .webview_windows()
                .into_iter()
                .filter(|(label, _)| label.starts_with("props-"))
                .collect();
            if !open.is_empty() {
                for (_, w) in open {
                    let _ = w.close();
                }
                return;
            }
            match crate::wallpaper::active_items(app.clone()) {
                Ok(ids) if !ids.is_empty() => {
                    if let Err(e) = crate::props_window::open(app, &ids[0]) {
                        tracing::warn!("hotkey open props: {e}");
                    }
                }
                _ => crate::main_window::ensure_main_window(app),
            }
        }
        "toggle_pause" => {
            let paused = app
                .try_state::<crate::wallpaper::WallpaperEngineState>()
                .map(|s| *s.paused.lock().unwrap())
                .unwrap_or(false);
            let r = if paused {
                crate::wallpaper::resume_all(app.clone())
            } else {
                crate::wallpaper::pause_all(app.clone())
            };
            if let Err(e) = r {
                tracing::warn!("hotkey toggle pause: {e}");
            }
        }
        "toggle_auto_pause" => {
            let cur = crate::tray_read_setting(app, "wallpaper_auto_pause", "false");
            let next = !(cur == "true" || cur == "1");
            if let Some(db) = app.try_state::<std::sync::Arc<Mutex<rusqlite::Connection>>>() {
                if let Ok(conn) = db.lock() {
                    let _ = crate::db::set_setting(
                        &conn,
                        "wallpaper_auto_pause",
                        if next { "true" } else { "false" },
                    );
                }
            }
            crate::notify_setting_changed(
                app,
                "wallpaper_auto_pause",
                if next { "true" } else { "false" },
            );
            // 同托盘：关闭时若正挂在自动暂停上，立即恢复播放
            if !next {
                if let Some(st) = app.try_state::<crate::wallpaper::WallpaperEngineState>() {
                    if !st.auto_paused.lock().unwrap().is_empty() {
                        let _ = crate::wallpaper::resume_all(app.clone());
                    }
                }
            }
        }
        "toggle_rotation" => {
            let paused = app
                .try_state::<std::sync::Arc<Mutex<rusqlite::Connection>>>()
                .and_then(|db| {
                    db.lock()
                        .ok()
                        .and_then(|c| crate::db::get_setting(&c, "playlist_rotation_paused"))
                })
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false);
            if let Err(e) = crate::wallpaper::rotation_set(app.clone(), !paused) {
                tracing::warn!("hotkey toggle rotation: {e}");
            }
        }
        "next_wallpaper" | "prev_wallpaper" => {
            // 需求：仅定时切换开启时有效。开启 = 有轮播在跑且未暂停
            let st = crate::wallpaper::playlist_status(app.clone())
                .unwrap_or_else(|_| serde_json::json!({ "active": false, "paused": false }));
            let running = st.get("active").and_then(|v| v.as_bool()).unwrap_or(false)
                && !st.get("paused").and_then(|v| v.as_bool()).unwrap_or(true);
            if !running {
                tracing::debug!("hotkey {action}: 定时切换未开启，忽略");
                return;
            }
            let r = if action == "next_wallpaper" {
                crate::wallpaper::next(app.clone(), None)
            } else {
                crate::wallpaper::prev(app.clone(), None)
            };
            if let Err(e) = r {
                tracing::warn!("hotkey {action}: {e}");
            }
        }
        _ => tracing::warn!("unknown hotkey action: {action}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_defaults() {
        for (id, _) in ACTIONS {
            assert!(!default_bindings(id).is_empty(), "{id} 缺默认绑定");
        }
    }

    #[test]
    fn main_window_has_global_binding() {
        // ⌘M/⌘H 是菜单键位（仅应用内）；主窗口隐藏后要能从任意应用唤回，
        // 必须至少有一枚**全局**绑定
        let globals: Vec<String> = default_bindings("toggle_main_window")
            .into_iter()
            .filter(|a| !is_mac_menu_accel(a))
            .collect();
        assert!(!globals.is_empty(), "主窗口显示/隐藏 缺全局绑定");
    }

    #[test]
    fn menu_accel_only_for_bare_cmd_key() {
        assert!(is_mac_menu_accel("cmd+m"));
        assert!(is_mac_menu_accel("cmd+h"));
        assert!(is_mac_menu_accel("cmd+p")); // ⌘+单键一律菜单级（不劫持其它 App）
        assert!(!is_mac_menu_accel("cmd+shift+m")); // 多修饰 → 全局
        assert!(!is_mac_menu_accel("ctrl+m")); // 非 ⌘ 修饰 → 全局
    }

    #[test]
    fn validate_rejects_bare_key_and_garbage() {
        assert!(validate_accel("cmd+shift+p").is_ok());
        assert!(validate_accel("f5").is_ok()); // F 键可以裸按
        assert!(validate_accel("p").is_err()); // 普通键必须带修饰
        assert!(validate_accel("cmd+").is_err());
        assert!(validate_accel("cmd+shift+notakey").is_err());
    }

    #[test]
    fn menu_accel_formats_for_muda() {
        assert_eq!(menu_accel("cmd+shift+s"), "Cmd+Shift+S");
        assert_eq!(menu_accel("cmd+m"), "Cmd+M");
    }
}

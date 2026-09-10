//! 其他平台的「正在播放」桩：媒体集成不可用，壁纸按「无媒体」渲染。

use tauri::AppHandle;

use super::MediaCommand;

pub fn start(_app: &AppHandle) {}

pub fn stop() {}

pub fn send_command(_cmd: MediaCommand) -> Result<(), String> {
    Err("当前平台不支持媒体控制".into())
}

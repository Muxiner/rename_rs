// 程序入口，非 Windows 调试模式下隐藏控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    rename_rust_lib::run()
}

// Windows のリリースビルドで余分なコンソールウィンドウを出さない
// (apps/chronogazer/src-tauri/src/main.rs と同じ)。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    banto_hub_shell_lib::run()
}

//! Tauri 的构建步骤：读 `tauri.conf.json` 和 `capabilities/`，生成权限表和 Windows 资源（图标、清单）。

fn main() {
    // 把应用自己的命令登记进权限表。登记之后，这些命令和插件的命令一样要在 capabilities 里授权才能调用；
    // 不登记的话，任何本地页面都能调。
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "engine_request",
            "open_document_dialog",
            "open_documents",
        ]));
    if let Err(error) = tauri_build::try_build(attributes) {
        panic!("tauri-build 失败：{error:#}");
    }
}

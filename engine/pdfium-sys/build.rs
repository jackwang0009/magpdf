//! 链接 PDFium 预编译库，并把 `pdfium.dll` 复制到可执行文件旁边。
//!
//! 预编译库目录默认为 `third_party/pdfium-prebuilt/`（由 `scripts/fetch-pdfium-prebuilt.ps1` 拉取），
//! 可用环境变量 `PDFIUM_PREBUILT_DIR` 覆盖，例如指向本地构建的 PDFium 分支产物。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=PDFIUM_PREBUILT_DIR");

    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR"));
    let prebuilt = env::var_os("PDFIUM_PREBUILT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            manifest_dir
                .join("..")
                .join("..")
                .join("third_party")
                .join("pdfium-prebuilt")
        });

    let lib_dir = prebuilt.join("lib");
    let import_lib = lib_dir.join("pdfium.dll.lib");
    let dll = prebuilt.join("bin").join("pdfium.dll");
    let version_file = prebuilt.join("VERSION");
    println!("cargo:rerun-if-changed={}", version_file.display());
    println!("cargo:rerun-if-changed={}", dll.display());

    if !import_lib.is_file() || !dll.is_file() {
        panic!(
            "找不到 PDFium 预编译库（{}）。请先运行 scripts/fetch-pdfium-prebuilt.ps1，\
             或用 PDFIUM_PREBUILT_DIR 指向包含 bin/pdfium.dll 与 lib/pdfium.dll.lib 的目录。",
            prebuilt.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=pdfium.dll");

    let version = read_version(&version_file);
    println!("cargo:rustc-env=PDFIUM_PREBUILT_VERSION={version}");
    println!("cargo:version={version}");
    println!("cargo:prebuilt_dir={}", prebuilt.display());

    copy_dll_next_to_binaries(&dll);
}

fn env_var(name: &str) -> String {
    match env::var(name) {
        Ok(v) => v,
        Err(e) => panic!("环境变量 {name} 不可用：{e}"),
    }
}

/// `VERSION` 文件形如 `MAJOR=159\nMINOR=0\nBUILD=8138\nPATCH=0`，拼成 `159.0.8138.0`。
fn read_version(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return String::from("unknown");
    };
    let mut parts = [None, None, None, None];
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let slot = match key.trim() {
            "MAJOR" => 0,
            "MINOR" => 1,
            "BUILD" => 2,
            "PATCH" => 3,
            _ => continue,
        };
        parts[slot] = Some(value.trim().to_owned());
    }
    if parts.iter().all(Option::is_some) {
        parts
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(".")
    } else {
        String::from("unknown")
    }
}

/// `OUT_DIR` 形如 `<target>/<profile>/build/pdfium-sys-<hash>/out`，往上三级是 `<target>/<profile>`。
/// 测试和示例程序分别在 `deps/` 和 `examples/` 下运行，所以这几处都放一份。
fn copy_dll_next_to_binaries(dll: &Path) {
    let out_dir = PathBuf::from(env_var("OUT_DIR"));
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    for dir in [
        profile_dir.to_path_buf(),
        profile_dir.join("deps"),
        profile_dir.join("examples"),
    ] {
        if fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let dest = dir.join("pdfium.dll");
        if needs_copy(dll, &dest)
            && let Err(e) = fs::copy(dll, &dest)
        {
            println!(
                "cargo:warning=复制 pdfium.dll 到 {} 失败：{e}",
                dest.display()
            );
        }
    }
}

fn needs_copy(src: &Path, dest: &Path) -> bool {
    let (Ok(s), Ok(d)) = (fs::metadata(src), fs::metadata(dest)) else {
        return true;
    };
    s.len() != d.len() || s.modified().ok() != d.modified().ok()
}

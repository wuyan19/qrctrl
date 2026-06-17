//! Windows 构建脚本：通过 `winres` 把 `assets/windows/icon.ico` 嵌入到 .exe 资源段。
//!
//! 非 Windows 平台直接 no-op（`winres` 也只在 Windows target 下 compile() 才做事，
//! 这里 cfg 提前跳过是为了让 `cargo build` 在 macOS/Linux 不报缺 rc.exe 之类）。
//!
//! ICO 文件由 `cargo run --example gen_icon` 从 `assets/icon.png` 生成。源图改了
//! 重跑那个 example，然后正常 `cargo build` 就会自动 pick 新的 .ico（build.rs 的
//! `cargo:rerun-if-changed` 在 winres 内部已经声明）。

#[cfg(target_os = "windows")]
fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/windows/icon.ico");
    res.set("FileDescription", "qrctrl — 用手机扫码控制 PC");
    res.set("ProductName", "qrctrl");
    // LegalCopyright 留空也能让 Windows 资源段合法；按需补充。
    if let Err(e) = res.compile() {
        // 编译失败不要 panic：CI 上偶尔有缺 rc.exe 的环境，CI 仍能产出可运行的二进制，
        // 只是没图标。本地开发安装 Windows SDK / Build Tools 后就不会触发。
        eprintln!("cargo:warning=winres compile 失败（.exe 将无图标）：{}", e);
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {}

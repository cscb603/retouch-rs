// Windows 资源注入：把图标(.ico)和 DPI manifest 编进 exe，
// 让 Explorer 里显示程序图标、高分屏下不糊。
//
// 注意：build script 是按「宿主」编译的，#[cfg(target_os="windows")] 在
// 交叉编译（macOS → windows-msvc）时并不会触发，所以这里改为运行时读
// Cargo 注入的 TARGET 环境变量来判断目标平台，更稳妥。
use std::path::PathBuf;

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let ico = manifest_dir.join("assets/app-icon.ico");
    let rc_path = out_dir.join("retouch.rc");
    let res_path = out_dir.join("retouch.res");
    let manifest_path = out_dir.join("retouch.manifest");

    // DPI 感知 manifest：声明支持 Win10/11 + per-monitor v2，避免系统拉伸糊化。
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{e2011457-1546-43c5-a5fe-008deee3d3f0}"/>
      <supportedOS Id="{fdbaa40d-ff49-4684-af34-1041a1499b7c}"/>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">permonitorv2,permonitor</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>"#;
    std::fs::write(&manifest_path, manifest).expect("写 manifest 失败");

    // 写 .rc：图标 + manifest 资源
    let rc = format!(
        "1 ICON \"{}\"\n1 RT_MANIFEST \"{}\"\n",
        ico.to_str().unwrap(),
        manifest_path.to_str().unwrap()
    );
    std::fs::write(&rc_path, rc).expect("写 .rc 失败");

    // 交叉编译时用 RC 环境变量指向 brew 的 llvm-rc（cargo-xwin 不带 rc）。
    let rc_compiler = std::env::var("RC")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/opt/homebrew/opt/llvm/bin/llvm-rc".to_string());

    // 注意：这版 llvm-rc 在 macOS 上用「绝对路径」调用会误判成多个输入文件而报错
    // （"Exactly one input file should be provided"）。 workaround：先切到 OUT_DIR，
    // 再用相对文件名 retouch.rc 调用，输出同名 retouch.res 落在 OUT_DIR。
    std::env::set_current_dir(&out_dir).expect("无法切换到 OUT_DIR");
    let out = std::process::Command::new(&rc_compiler)
        .arg("retouch.rc")
        .output()
        .unwrap_or_else(|e| panic!("无法启动 rc 编译器 ({}): {}", rc_compiler, e));
    assert!(
        out.status.success(),
        "rc 编译资源失败（stderr: {}）",
        String::from_utf8_lossy(&out.stderr)
    );

    // 把 .res 作为链接输入编进 exe（lld-link / link 都支持 .res 输入）。
    println!(
        "cargo:rustc-link-arg-bin=retouch-rs-gui={}",
        res_path.to_str().unwrap()
    );
}

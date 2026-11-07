fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/AppIcon.ico");
    println!("cargo:rerun-if-changed=packaging/windows/app.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_icon();
    }
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/launcher.slint", config)
        .expect("Unable to compile launcher UI");
}

fn embed_windows_icon() {
    use std::{env, path::PathBuf, process::Command};

    let target = env::var("TARGET").expect("missing target triple");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("missing output directory"));
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let output = out_dir.join(if msvc { "app-icon.res" } else { "app-icon.o" });
    let mut command = if msvc {
        cc::windows_registry::find(&target, "rc.exe").unwrap_or_else(|| Command::new("rc.exe"))
    } else {
        let compiler = env::var("WINDRES").unwrap_or_else(|_| {
            if env::var("HOST").as_deref() == Ok(target.as_str()) {
                "windres".into()
            } else {
                format!("{target}-windres")
            }
        });
        Command::new(compiler)
    };
    if msvc {
        command
            .arg("/nologo")
            .arg(format!("/fo{}", output.display()));
    } else {
        command.arg("-O").arg("coff").arg("-o").arg(&output);
    }
    let status = command
        .arg("packaging/windows/app.rc")
        .status()
        .expect("failed to start Windows resource compiler");
    assert!(
        status.success(),
        "Windows resource compiler failed: {status}"
    );
    println!("cargo:rustc-link-arg-bin=take-a-panel={}", output.display());
}

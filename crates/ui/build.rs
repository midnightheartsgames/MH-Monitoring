//! Ресурсы исполняемого файла: иконка, сведения о версии, манифест (PLAN.md §6/P7).
//!
//! `.rc` собирается здесь, а не лежит файлом: номер версии берётся из `Cargo.toml` и не может
//! разойтись с тем, что программа пишет в журнал.
//!
//! Сюда же встраивается `MH-Monitoring-Service.exe` (PLAN.md §6/P9): дистрибутив остаётся одним
//! файлом, а установщик раскладывает службу рядом. Файл берётся из `MH_SERVICE_EXE` или из той же
//! папки сборки. Порядок сборки гарантирует `tools\build-release.ps1`: сначала служба, потом UI.

fn main() {
    embed_service();
    #[cfg(windows)]
    windows_resources();
}

/// Копирует службу в `OUT_DIR/service.bin`. Нет файла — пустой: такая сборка работает, но
/// установить службу из встроенной копии не может и честно об этом говорит.
fn embed_service() {
    use std::path::PathBuf;

    const NAME: &str = "MH-Monitoring-Service.exe";
    println!("cargo:rerun-if-env-changed=MH_SERVICE_EXE");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR = target\<профиль>\build\mh-ui-<хеш>\out
    let source = match std::env::var_os("MH_SERVICE_EXE") {
        Some(path) => PathBuf::from(path),
        None => match out_dir.ancestors().nth(3) {
            Some(profile_dir) => profile_dir.join(NAME),
            None => PathBuf::from(NAME),
        },
    };
    println!("cargo:rerun-if-changed={}", source.display());
    let bytes = std::fs::read(&source).unwrap_or_default();
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    if bytes.is_empty() && windows {
        println!(
            "cargo:warning=служба не встроена: нет {}; собирайте через tools\\build-release.ps1",
            source.display()
        );
    }
    std::fs::write(out_dir.join("service.bin"), bytes).unwrap();
}

#[cfg(windows)]
fn windows_resources() {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let icon = manifest_dir.join("../../assets/icon/MH-Monitoring.ico");
    let manifest = manifest_dir.join("MH-Monitoring.manifest");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", manifest.display());

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u32> = version.split(['.', '-']).filter_map(|p| p.parse().ok()).collect();
    parts.resize(4, 0);
    let numeric = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);
    let escape = |path: &PathBuf| path.display().to_string().replace('\\', "\\\\");

    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "041904B0"
    BEGIN
      VALUE "CompanyName", "midnightheartsgames"
      VALUE "FileDescription", "MH Monitoring — оверлей производительности"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "MH-Monitoring"
      VALUE "OriginalFilename", "MH-Monitoring.exe"
      VALUE "ProductName", "MH Monitoring"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x419, 1200
  END
END
"#,
        icon = escape(&icon),
        manifest = escape(&manifest),
    );
    let rc_path = out_dir.join("MH-Monitoring.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_required().unwrap();
}

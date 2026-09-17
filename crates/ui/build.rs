//! Ресурсы исполняемого файла: иконка, сведения о версии, манифест (PLAN.md §6/P7).
//!
//! `.rc` собирается здесь, а не лежит файлом: номер версии берётся из `Cargo.toml` и не может
//! разойтись с тем, что программа пишет в журнал.

fn main() {
    #[cfg(windows)]
    windows_resources();
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

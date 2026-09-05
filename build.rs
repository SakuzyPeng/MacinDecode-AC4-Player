use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const FONT_FILE_NAME: &str = "NotoSansCJKsc-Regular.otf";
const FONT_OVERRIDE_ENV: &str = "MACINDECODE_UI_FONT_PATH";
const FONT_SHA256: &str = "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b";
const FONT_URL: &str = "https://cdn.jsdelivr.net/gh/notofonts/noto-cjk@165c01b46ea533872e002e0785ff17e44f6d97d8c/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed={FONT_OVERRIDE_ENV}");

    declare_spatial_output();

    if let Err(error) = prepare_ui_font() {
        panic!("failed to prepare the UI font: {error}");
    }
}

/// Derive the decode/native gates in one place. `windows_spatial_output`
/// enables the existing COM/object path; `macinrender_output` enables the
/// optional C ABI on macOS/Windows; `spatial_output` is their union. Shared
/// Scene arithmetic and preview remain gated only by `decode`.
fn declare_spatial_output() {
    println!("cargo::rustc-check-cfg=cfg(spatial_output)");
    println!("cargo::rustc-check-cfg=cfg(windows_spatial_output)");
    println!("cargo::rustc-check-cfg=cfg(macinrender_output)");
    let windows = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let macos = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let decode = env::var_os("CARGO_FEATURE_DECODE").is_some();
    let macinrender =
        decode && (windows || macos) && env::var_os("CARGO_FEATURE_MACINRENDER").is_some();
    if windows && decode {
        println!("cargo::rustc-cfg=windows_spatial_output");
    }
    if macinrender {
        println!("cargo::rustc-cfg=macinrender_output");
    }
    if (windows && decode) || macinrender {
        println!("cargo::rustc-cfg=spatial_output");
    }
}

fn prepare_ui_font() -> Result<(), String> {
    let out_dir = env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "Cargo did not provide OUT_DIR".to_owned())?;
    let destination = out_dir.join(FONT_FILE_NAME);

    if let Some(source) = env::var_os(FONT_OVERRIDE_ENV).map(PathBuf::from) {
        println!("cargo:rerun-if-changed={}", source.display());
        let bytes = read_verified_font(&source)?;
        fs::write(&destination, bytes).map_err(|error| {
            format!(
                "could not copy {} to {}: {error}",
                source.display(),
                destination.display()
            )
        })?;
        return Ok(());
    }

    if destination.is_file() && read_verified_font(&destination).is_ok() {
        return Ok(());
    }

    let partial = destination.with_extension("otf.part");
    if partial.exists() {
        fs::remove_file(&partial)
            .map_err(|error| format!("could not remove stale {}: {error}", partial.display()))?;
    }
    println!("cargo:warning=Downloading checksum-pinned Noto Sans CJK SC for the UI");
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--compressed",
            "--connect-timeout",
            "20",
            "--max-time",
            "300",
            "--retry",
            "2",
            "--retry-all-errors",
        ])
        .arg("--output")
        .arg(&partial)
        .arg(FONT_URL)
        .status()
        .map_err(|error| {
            format!("could not start curl ({error}); install curl or set {FONT_OVERRIDE_ENV}")
        })?;
    if !status.success() {
        return Err(format!(
            "curl could not download {FONT_URL}; set {FONT_OVERRIDE_ENV} for an offline build"
        ));
    }

    read_verified_font(&partial)?;
    if destination.exists() {
        fs::remove_file(&destination).map_err(|error| {
            format!(
                "could not replace invalid {}: {error}",
                destination.display()
            )
        })?;
    }
    fs::rename(&partial, &destination).map_err(|error| {
        format!(
            "could not move {} to {}: {error}",
            partial.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn read_verified_font(path: &Path) -> Result<Vec<u8>, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let digest = Sha256::digest(&bytes);
    let mut actual = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut actual, "{byte:02x}")
            .map_err(|error| format!("could not format the font checksum: {error}"))?;
    }
    if actual != FONT_SHA256 {
        return Err(format!(
            "{} has SHA-256 {actual}, expected {FONT_SHA256}",
            path.display()
        ));
    }
    Ok(bytes)
}

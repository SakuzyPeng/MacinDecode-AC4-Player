use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

mod native_link;

fn run(command: &mut Command) {
    let status = command
        .status()
        .expect("cannot start CMake; install CMake and a C++20 compiler");
    assert!(
        status.success(),
        "MacinRender native build failed: {command:?}"
    );
}

fn main() {
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=native_link.rs");
    println!("cargo:rustc-check-cfg=cfg(native_macinrender)");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    if os != "macos" && os != "windows" {
        return;
    }
    if os == "macos" {
        compile_atmos_assist();
    }
    println!("cargo:rustc-cfg=native_macinrender");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("native");
    let query = out.join(".cmake/api/v1/query");
    fs::create_dir_all(&query).unwrap();
    fs::write(query.join("codemodel-v2"), "").unwrap();
    let mut configure = Command::new("cmake");
    configure.arg("-DMACINRENDER_SOURCE_DIR=");
    // cc locates MSVC and the Windows SDK even outside a developer shell. Keep
    // CMake configure and build in the same compiler environment.
    let compiler = (os == "windows").then(|| cc::Build::new().cpp(true).get_compiler());
    if let Some(compiler) = &compiler {
        if let Some(directory) = compiler.path().parent() {
            println!("cargo:compiler_dir={}", directory.display());
        }
        configure.envs(compiler.env().iter().cloned());
        configure.arg(format!("-DCMAKE_C_COMPILER={}", compiler.path().display()));
        configure.arg(format!(
            "-DCMAKE_CXX_COMPILER={}",
            compiler.path().display()
        ));
    }
    configure.args(["-S", "native", "-B"]).arg(&out);
    configure.args(["-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release"]);
    if os == "macos" {
        configure.arg("-DCMAKE_OSX_DEPLOYMENT_TARGET=14.0");
        let arch = if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
            "arm64"
        } else {
            "x86_64"
        };
        configure.arg(format!("-DCMAKE_OSX_ARCHITECTURES={arch}"));
    }
    for (variable, option) in [
        ("MACINRENDER_SOURCE_DIR", "MACINRENDER_SOURCE_DIR"),
        ("MACINRENDER_FETCHCONTENT_DIR", "FETCHCONTENT_BASE_DIR"),
        ("CMAKE_TOOLCHAIN_FILE", "CMAKE_TOOLCHAIN_FILE"),
        ("OPENBLAS_LIBRARY", "OPENBLAS_LIBRARY"),
        ("LAPACKE_LIBRARY", "LAPACKE_LIBRARY"),
        ("OPENBLAS_HEADER_PATH", "OPENBLAS_HEADER_PATH"),
        ("LAPACKE_HEADER_PATH", "LAPACKE_HEADER_PATH"),
        ("BOOST_ROOT", "BOOST_ROOT"),
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
        if let Some(value) = env::var_os(variable) {
            configure.arg(format!("-D{option}={}", value.to_string_lossy()));
            if variable == "MACINRENDER_SOURCE_DIR" {
                for directory in ["src", "include", "gui/native", "CMakeLists.txt"] {
                    println!(
                        "cargo:rerun-if-changed={}",
                        PathBuf::from(&value).join(directory).display()
                    );
                }
            }
        }
    }
    run(&mut configure);
    let mut build = Command::new("cmake");
    if let Some(compiler) = &compiler {
        build.envs(compiler.env().iter().cloned());
    }
    build
        .arg("--build")
        .arg(&out)
        .args(["--target", "macinrender_link_probe"]);
    build
        .arg("--parallel")
        .arg(env::var("NUM_JOBS").unwrap_or_else(|_| "4".into()));
    run(&mut build);
    let source = fs::read_to_string(out.join("macinrender-source.txt")).unwrap();
    let binary = fs::read_to_string(out.join("macinrender-binary.txt")).unwrap();
    native_link::emit(&out, os == "windows");
    run(&mut Command::new(out.join(if os == "windows" {
        "macinrender_link_probe.exe"
    } else {
        "macinrender_link_probe"
    })));
    println!("cargo:lib_dir={binary}");
    println!("cargo:source_dir={source}");
    println!("cargo:linkage=static");
    println!(
        "cargo:link_manifest={}",
        out.join("native-link.json").display()
    );
    cc::Build::new()
        .file("native/abi_probe.c")
        .include(PathBuf::from(&source).join("include"))
        .include(PathBuf::from(source).join("gui/native"))
        .compile("macinrender_abi_probe");
}

fn compile_atmos_assist() {
    println!("cargo:rerun-if-changed=../../assets/audio/atmos-assist.m4a");
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .flag("-fobjc-arc")
        .flag("-fblocks")
        .flag("-mmacosx-version-min=14.0")
        .file("native/atmos_assist.mm")
        .compile("macindecode_atmos_assist");
    for framework in [
        "AVFoundation",
        "Foundation",
        "MediaToolbox",
        "CoreMedia",
        "AudioToolbox",
        "CoreAudio",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}

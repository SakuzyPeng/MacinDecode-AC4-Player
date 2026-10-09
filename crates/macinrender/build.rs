use std::env;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
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

fn cmake_command() -> Command {
    let mut command = Command::new("cmake");
    // Corrosion's Cargo probes must not inherit the outer Cargo target lock
    // or jobserver file descriptors. Core shares its own Rust target directory.
    command
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_MAKEFLAGS");
    if let Some(jobs) = env::var_os("MACINRENDER_BUILD_JOBS") {
        command.env("CARGO_BUILD_JOBS", jobs);
    }
    command
}

fn native_directory(compiler: Option<&cc::Tool>) -> PathBuf {
    println!("cargo:rerun-if-env-changed=MACINRENDER_BUILD_DIR");
    let Some(root) = env::var_os("MACINRENDER_BUILD_DIR").filter(|value| !value.is_empty()) else {
        return PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("native");
    };
    let root = PathBuf::from(root);
    assert!(root.is_absolute(), "MACINRENDER_BUILD_DIR must be absolute");
    // CMake always builds Release. Rust's test/check/release profiles can share
    // it, but different source trees, targets and compiler settings cannot.
    // A short digest keeps the Windows object paths below MAX_PATH.
    let mut identity = DefaultHasher::new();
    env::var_os("CARGO_MANIFEST_DIR").hash(&mut identity);
    compiler.map(cc::Tool::path).hash(&mut identity);
    for variable in [
        "TARGET",
        "HOST",
        "CC",
        "CXX",
        "CFLAGS",
        "CXXFLAGS",
        "CMAKE_TOOLCHAIN_FILE",
        "MACINRENDER_SOURCE_DIR",
        "MACINRENDER_FETCHCONTENT_DIR",
        "DEVELOPER_DIR",
        "SDKROOT",
        "MACOSX_DEPLOYMENT_TARGET",
        "RUSTUP_TOOLCHAIN",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
        env::var_os(variable).hash(&mut identity);
    }
    root.join(format!("{:016x}", identity.finish()))
}

fn lock_native_build(out: &std::path::Path) -> fs::File {
    fs::create_dir_all(out).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(out.join(".build.lock"))
        .unwrap();
    lock.lock().expect("cannot lock the native build directory");
    lock
}

fn configure_native_sources(configure: &mut Command) {
    for (variable, option) in [
        ("MACINRENDER_SOURCE_DIR", "MACINRENDER_SOURCE_DIR"),
        ("MACINRENDER_FETCHCONTENT_DIR", "FETCHCONTENT_BASE_DIR"),
        ("CMAKE_TOOLCHAIN_FILE", "CMAKE_TOOLCHAIN_FILE"),
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
        if let Some(value) = env::var_os(variable) {
            configure.arg(format!("-D{option}={}", value.to_string_lossy()));
            if variable == "MACINRENDER_SOURCE_DIR" {
                for directory in [
                    "src",
                    "include",
                    "gui/native",
                    "CMakeLists.txt",
                    "cmake",
                    "rust",
                ] {
                    println!(
                        "cargo:rerun-if-changed={}",
                        PathBuf::from(&value).join(directory).display()
                    );
                }
            }
        }
    }
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
    println!("cargo:rerun-if-env-changed=MACINRENDER_BUILD_JOBS");
    let compiler = (os == "windows").then(|| cc::Build::new().cpp(true).get_compiler());
    let out = native_directory(compiler.as_ref());
    let _native_lock = lock_native_build(&out);
    let query = out.join(".cmake/api/v1/query");
    fs::create_dir_all(&query).unwrap();
    fs::write(query.join("codemodel-v2"), "").unwrap();
    let mut configure = cmake_command();
    configure.arg("-DMACINRENDER_SOURCE_DIR=");
    // cc locates MSVC and the Windows SDK even outside a developer shell. Keep
    // CMake configure and build in the same compiler environment.
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
    configure_native_sources(&mut configure);
    run(&mut configure);
    let mut build = cmake_command();
    if let Some(compiler) = &compiler {
        build.envs(compiler.env().iter().cloned());
    }
    build
        .arg("--build")
        .arg(&out)
        .args(["--target", "macinrender_link_probe"]);
    build.arg("--parallel").arg(
        env::var("MACINRENDER_BUILD_JOBS")
            .or_else(|_| env::var("NUM_JOBS"))
            .unwrap_or_else(|_| "4".into()),
    );
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
        .std("c11")
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

"""Build a checksum-pinned, MSVC-ABI OpenBLAS SDK with no redistributable DLLs."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile

VERSION = "0.3.34"
SOURCE_SHA = "cd7e129868320cc2d033afa920e31202dfe0b8066a5b66661900ccc0f197dfed"
LLVM_VERSION = "21.1.8"
LLVM_SHA = "7a5386c26497db1691f320121e5b113364dd0274b98e55f15f4dbc00c0450113"
OPTIONS = {
    "CMAKE_BUILD_TYPE": "Release", "CMAKE_MSVC_RUNTIME_LIBRARY": "MultiThreaded",
    "MSVC_STATIC_CRT": "ON", "BUILD_STATIC_LIBS": "ON", "BUILD_SHARED_LIBS": "OFF",
    "NOFORTRAN": "1", "C_LAPACK": "ON", "BUILD_WITHOUT_LAPACK": "OFF",
    "BUILD_WITHOUT_LAPACKE": "OFF", "INTERFACE64": "OFF", "TARGET": "GENERIC",
    "BINARY": "64", "DYNAMIC_ARCH": "ON", "DYNAMIC_OLDER": "ON",
    "USE_THREAD": "ON", "NUM_THREADS": "64", "USE_OPENMP": "OFF", "BUILD_TESTING": "OFF",
    "CMAKE_C_FLAGS_RELEASE": "/O2 /Ob2 /DNDEBUG /clang:-Wno-everything",
}


def run(arguments, **kwargs):
    return subprocess.run(list(map(str, arguments)), check=True, **kwargs)


def digest(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def toolchain(llvm):
    import winreg
    env = {key.upper(): value for key, value in os.environ.items()}
    vswhere = Path(env["PROGRAMFILES(X86)"]) / "Microsoft Visual Studio/Installer/vswhere.exe"
    vs = Path(run([vswhere, "-latest", "-products", "*", "-requires",
                   "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath"],
                  text=True, capture_output=True).stdout.strip())
    version_key = lambda value: tuple(int(part) for part in value.split("."))
    vc = max((vs / "VC/Tools/MSVC").iterdir(), key=lambda path: version_key(path.name))
    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows Kits\Installed Roots") as key:
        sdk = Path(winreg.QueryValueEx(key, "KitsRoot10")[0])
    sdk_version = max((path.name for path in (sdk / "Include").iterdir() if (path / "um").is_dir()), key=version_key)
    env["INCLUDE"] = ";".join(map(str, [vc / "include", *[sdk / "Include" / sdk_version / name for name in ("ucrt", "shared", "um", "winrt")]]))
    env["LIB"] = ";".join(map(str, [vc / "lib/x64", sdk / "Lib" / sdk_version / "ucrt/x64", sdk / "Lib" / sdk_version / "um/x64"]))
    env["PATH"] = ";".join(map(str, [llvm / "bin", vc / "bin/Hostx64/x64", sdk / "bin" / sdk_version / "x64"])) + ";" + env["PATH"]
    return env, {"llvm": LLVM_VERSION, "msvc": vc.name, "windows_sdk": sdk_version}


def prepare(root, download):
    from verify_runtime import pe_imports, require, verify_windows_imports

    cache = root / ".ci-tools/static"
    cache.mkdir(parents=True, exist_ok=True)
    llvm_archive = cache / f"LLVM-{LLVM_VERSION}-win64.exe"
    download(f"https://github.com/llvm/llvm-project/releases/download/llvmorg-{LLVM_VERSION}/{llvm_archive.name}", llvm_archive, LLVM_SHA)
    llvm = cache / f"llvm-{LLVM_VERSION}"
    if not (llvm / "bin/clang-cl.exe").is_file():
        seven = shutil.which("7z") or str(Path(os.environ["ProgramFiles"]) / "7-Zip/7z.exe")
        run([seven, "x", "-y", f"-o{llvm}", llvm_archive], stdout=subprocess.DEVNULL)
    reported = run([llvm / "bin/clang-cl.exe", "--version"], capture_output=True, text=True).stdout.splitlines()[0]
    require(reported == f"clang version {LLVM_VERSION}" or reported.startswith(f"clang version {LLVM_VERSION} "), "LLVM version mismatch")
    archive = cache / f"OpenBLAS-{VERSION}.tar.gz"
    download(f"https://github.com/OpenMathLib/OpenBLAS/releases/download/v{VERSION}/{archive.name}", archive, SOURCE_SHA)
    source = cache / f"OpenBLAS-{VERSION}"
    if not (source / "CMakeLists.txt").is_file():
        with tarfile.open(archive) as tar:
            tar.extractall(cache, filter="data")
    env, compiler = toolchain(llvm)
    # Keep ambient MSVC flags out of the pinned build. In particular, _CL_
    # cannot append flags to OpenBLAS assembly commands after their '--'.
    for variable in ("CL", "_CL_", "CPATH", "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "LIBRARY_PATH", "CFLAGS", "CXXFLAGS", "LDFLAGS"):
        env.pop(variable, None)
    cmake = shutil.which("cmake")
    require(cmake is not None, "Install CMake 3.31.6")
    require("cmake version 3.31.6" in run([cmake, "--version"], capture_output=True, text=True).stdout, "CMake 3.31.6 is required")
    spec = {"version": VERSION, "source_sha256": SOURCE_SHA, "llvm_sha256": LLVM_SHA,
            "toolchain": compiler, "cmake": "3.31.6", "options": OPTIONS, "linkage": "static"}
    key = hashlib.sha256(json.dumps(spec, sort_keys=True).encode()).hexdigest()[:16]
    install = root / ".ci-inputs/openblas-static" / key
    manifest = install / "build.json"
    library = install / "lib/openblas.lib"
    probe_source = root / "scripts/native/openblas_probe.c"
    if manifest.is_file() and library.is_file():
        saved = json.loads(manifest.read_text())
        if saved.get("library_sha256") == digest(library) and saved.get("probe_sha256") == digest(probe_source):
            return settings(install)
    build = root / "target/openblas-static" / key
    build.mkdir(parents=True, exist_ok=True)
    log = build / "build.log"
    try:
        with log.open("w", encoding="utf-8") as output:
            flags = [f"-D{name}={value}" for name, value in OPTIONS.items()]
            flags += [f"-DCMAKE_C_COMPILER={llvm}/bin/clang-cl.exe", f"-DCMAKE_CXX_COMPILER={llvm}/bin/clang-cl.exe", f"-DCMAKE_INSTALL_PREFIX={install}"]
            run([cmake, "-S", source, "-B", build, "-G", "Ninja", *flags], env=env, stdout=output, stderr=subprocess.STDOUT)
            run([cmake, "--build", build, "--target", "openblas_static", "--parallel", os.getenv("OPENBLAS_BUILD_JOBS", "4")], env=env, stdout=output, stderr=subprocess.STDOUT)
            run([cmake, "--install", build], env=env, stdout=output, stderr=subprocess.STDOUT)
            probe = build / "openblas-probe.exe"
            run([llvm / "bin/clang-cl.exe", "/nologo", "/MT", "/O2", probe_source, f"/I{install / 'include/openblas'}",
                 f"/Fe:{probe}", f"/Fo:{build / 'probe.obj'}", "/link", library], env=env, stdout=output, stderr=subprocess.STDOUT)
        verify_windows_imports(pe_imports(probe, executable=False))
        # Keep CI's numerical check cheap while exercising OpenBLAS worker startup.
        result = run([probe], env=dict(env, OPENBLAS_NUM_THREADS="2"), text=True, capture_output=True, timeout=60)
        report = json.loads(result.stdout)
        require(report["ok"], "OpenBLAS numerical probe failed")
        spec.update(library_sha256=digest(library), probe_sha256=digest(probe_source), numerical_probe=report)
        manifest.write_text(json.dumps(spec, indent=2), encoding="utf-8")
        print(f"Verified static OpenBLAS {VERSION}: {library}", flush=True)
    except Exception as error:
        diagnostics = root / "target/packaging-failures/openblas"
        diagnostics.mkdir(parents=True, exist_ok=True)
        if log.exists():
            shutil.copy2(log, diagnostics / "build.log")
            print("\n".join(log.read_text(errors="replace").splitlines()[-60:]), flush=True)
        if isinstance(error, subprocess.CalledProcessError):
            print(error.stdout or "", error.stderr or "", flush=True)
        raise
    return settings(install)


def settings(install):
    return {"OPENBLAS_LIBRARY": str(install / "lib/openblas.lib"),
            "LAPACKE_LIBRARY": str(install / "lib/openblas.lib"),
            "OPENBLAS_HEADER_PATH": str(install / "include/openblas"),
            "LAPACKE_HEADER_PATH": str(install / "include/openblas"),
            "OPENBLAS_BUILD_MANIFEST": str(install / "build.json")}

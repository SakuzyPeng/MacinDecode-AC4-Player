"""Inspect the actual executable and run it without developer library paths."""
import ctypes
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import time

WINDOWS_SYSTEM_DLLS = set("""
advapi32.dll avrt.dll bcrypt.dll bcryptprimitives.dll cfgmgr32.dll combase.dll
comctl32.dll comdlg32.dll crypt32.dll d3d12.dll d3dcompiler_47.dll dcomp.dll
dwmapi.dll dxgi.dll gdi32.dll hid.dll imm32.dll kernel32.dll kernelbase.dll
mmdevapi.dll msvcrt.dll ntdll.dll ole32.dll oleaut32.dll opengl32.dll powrprof.dll
propsys.dll rpcrt4.dll secur32.dll setupapi.dll shell32.dll shlwapi.dll
uiautomationcore.dll user32.dll userenv.dll uxtheme.dll version.dll winmm.dll
winspool.drv wintrust.dll ws2_32.dll wtsapi32.dll
""".split())
MAC_SYSTEM_ROOTS = (
    "/System/Library/", "/usr/lib/", "/Library/Apple/System/Library/",
    "/System/Volumes/Preboot/Cryptexes/OS/usr/lib/",
    "/System/Volumes/Preboot/Cryptexes/OS/System/Library/",
    "/System/Cryptexes/OS/usr/lib/", "/System/Cryptexes/OS/System/Library/",
)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def pe_imports(binary, executable=True):
    data = Path(binary).read_bytes()
    require(data[:2] == b"MZ", "Not a PE executable")
    u16 = lambda offset: struct.unpack_from("<H", data, offset)[0]
    u32 = lambda offset: struct.unpack_from("<I", data, offset)[0]
    pe = u32(0x3C)
    require(data[pe:pe + 4] == b"PE\0\0", "Invalid PE signature")
    require(u16(pe + 4) == 0x8664, "Expected Windows x64")
    optional = pe + 24
    require(u16(optional) == 0x20B, "Expected PE32+")
    if executable: require(u16(optional + 68) == 2, "Release executable must use the Windows GUI subsystem")
    sections = []
    for index in range(u16(pe + 6)):
        position = optional + u16(pe + 20) + index * 40
        virtual_size, address, size, raw = struct.unpack_from("<IIII", data, position + 8)
        sections.append((address, max(virtual_size, size), raw))

    def offset(rva):
        for address, size, raw in sections:
            if address <= rva < address + size:
                value = raw + rva - address
                require(value < len(data), "PE address outside file")
                return value
        raise RuntimeError(f"Unmapped PE address: {rva}")

    def name(rva):
        position = offset(rva)
        return data[position:data.index(b"\0", position)].decode("ascii").lower()

    result = {"direct": [], "delay": []}
    directories = optional + 112
    for key, index, stride, name_offset in [("direct", 1, 20, 12), ("delay", 13, 32, 4)]:
        rva = u32(directories + index * 8)
        if not rva:
            continue
        position = offset(rva)
        while any(data[position:position + stride]):
            require(position + stride <= len(data), "Truncated PE import descriptor")
            if key == "delay":
                require(u32(position) & 1, "Unsupported delay import address format")
            result[key].append(name(u32(position + name_offset)))
            position += stride
    return result


def verify_binary(binary, target, bundle=None):
    binary = Path(binary)
    if target == "x86_64-pc-windows-msvc":
        imports = pe_imports(binary, binary.suffix.lower() == ".exe")
        for dependency in imports["direct"] + imports["delay"]:
            allowed = dependency in WINDOWS_SYSTEM_DLLS or dependency.startswith(("api-ms-win-core-", "ext-ms-win-"))
            if bundle is not None:
                allowed = allowed or dependency.startswith("api-ms-win-crt-") or any(file.name.lower() == dependency for file in Path(bundle).glob("*.dll"))
            require(allowed, f"Unexpected DLL dependency: {dependency}")
        return imports
    require(target == "aarch64-apple-darwin", f"Unsupported target: {target}")
    data = binary.read_bytes()
    require(struct.unpack_from("<II", data) == (0xFEEDFACF, 0x100000C), "Expected an ARM64 Mach-O executable")
    output = subprocess.check_output(["otool", "-L", str(binary)], text=True)
    dependencies = [line.strip().split(" (", 1)[0] for line in output.splitlines()[2 if binary.suffix == ".dylib" else 1:]]
    require(dependencies, "Mach-O dependency report is empty")
    for dependency in dependencies:
        require(dependency.startswith(MAC_SYSTEM_ROOTS), f"Non-system Mach-O dependency: {dependency}")
    commands = subprocess.check_output(["otool", "-l", str(binary)], text=True)
    for path in re.findall(r"path (.+) \(offset", commands):
        require(path.startswith(("@loader_path", "@executable_path", "/usr/lib")), f"Build-machine rpath: {path}")
    minimums = []
    for command in commands.split("Load command"):
        if "cmd LC_BUILD_VERSION\n" in command:
            minimums += re.findall(r"^\s*minos\s+(\d+\.\d+(?:\.\d+)?)\s*$", command, re.M)
        elif "cmd LC_VERSION_MIN_MACOSX\n" in command:
            minimums += re.findall(r"^\s*version\s+(\d+\.\d+(?:\.\d+)?)\s*$", command, re.M)
    require(minimums and all(tuple(map(int, value.split("."))) <= (14, 0, 0) for value in minimums),
            f"Mach-O deployment target exceeds macOS 14: {minimums}")
    return {"dependencies": dependencies}


def windows_modules(pid):
    from ctypes import wintypes

    class Module(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("module_id", wintypes.DWORD),
                    ("process_id", wintypes.DWORD), ("global_usage", wintypes.DWORD),
                    ("process_usage", wintypes.DWORD), ("base", ctypes.c_void_p),
                    ("image_size", wintypes.DWORD), ("handle", wintypes.HMODULE),
                    ("name", wintypes.WCHAR * 256), ("path", wintypes.WCHAR * 260)]

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel.Module32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Module)]
    kernel.Module32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Module)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    snapshot = kernel.CreateToolhelp32Snapshot(0x8 | 0x10, pid)
    if snapshot == ctypes.c_void_p(-1).value:
        return []  # Process may be between startup or shutdown loader states.
    entry = Module()
    entry.size = ctypes.sizeof(entry)
    paths = []
    try:
        more = kernel.Module32FirstW(snapshot, ctypes.byref(entry))
        while more:
            paths.append(entry.path)
            more = kernel.Module32NextW(snapshot, ctypes.byref(entry))
    finally:
        kernel.CloseHandle(snapshot)
    return paths


def within(path, root):
    path, root = os.path.normcase(os.path.realpath(path)), os.path.normcase(os.path.realpath(root))
    try:
        return os.path.commonpath([path, root]) == root
    except ValueError:
        return False


def run_smoke(binary, data_root):
    binary, data_root = Path(binary).resolve(), Path(data_root).resolve()
    data_root.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    for key in list(env):
        if key.startswith(("DYLD_", "WGPU_", "VK_")) or key in ("LD_LIBRARY_PATH", "LD_PRELOAD"):
            env.pop(key)
    if os.name == "nt":
        system = Path(env["SystemRoot"])
        env["PATH"] = os.pathsep.join(map(str, [system / "System32", system]))
    else:
        env["PATH"] = "/usr/bin:/bin:/usr/sbin:/sbin"
    subprocess.run([str(binary), "--check-install", "--data-dir", str(data_root)],
                   cwd=data_root, env=env, check=True, timeout=30, capture_output=True)
    check = json.loads((data_root / "install-check.json").read_text())
    require(check["ok"] and check["embedded_licenses"], "Installed executable lacks storage or embedded notices")
    require(check.get("decode") and check.get("macinrender"), "Installer must contain the full decoder and renderer")
    if sys.platform == "darwin":
        env["DYLD_PRINT_LIBRARIES"] = "1"
    modules = set()
    log = data_root / "window.log"
    with log.open("w", encoding="utf-8") as output:
        process = subprocess.Popen([str(binary), "--smoke-test", "--data-dir", str(data_root)],
                                   cwd=data_root, env=env, stdout=output, stderr=output)
        deadline = time.monotonic() + 30
        try:
            while process.poll() is None:
                if os.name == "nt":
                    modules.update(windows_modules(process.pid))
                require(time.monotonic() < deadline, f"Window smoke test timed out; see {log}")
                time.sleep(0.05)
            require(process.returncode == 0, f"Window failed ({process.returncode}); see {log}")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
    report = json.loads((data_root / "smoke-report.json").read_text())
    require(report["ok"] and report["rendered_frames"] >= 2, f"Window did not initialize: {report}")
    if sys.platform == "darwin":
        modules.update(re.findall(r"^dyld\[\d+\]:\s+(?:<[^>]+>\s+)?(/.+)$", log.read_text(), re.M))
    require(len(modules) > 1, "No runtime library evidence was collected")
    for module in modules:
        if Path(module).resolve() == binary:
            continue
        if os.name == "nt":
            bundled = Path(module).parent.resolve() == binary.parent and Path(module).suffix.lower() == ".dll"
            require(within(module, env["SystemRoot"]) or bundled, f"Runtime loaded an unpackaged module: {module}")
        else:
            bundled = within(module, binary.parents[2] / "Contents/Frameworks")
            require(module.startswith(MAC_SYSTEM_ROOTS) or bundled, f"Runtime loaded an unpackaged library: {module}")
    report["loaded_modules"] = sorted(modules)
    (data_root / "runtime-report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    return report

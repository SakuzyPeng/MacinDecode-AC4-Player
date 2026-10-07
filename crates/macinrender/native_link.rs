//! Translate `CMake`'s resolved link interface, including transitive static archives.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, PartialEq, Eq)]
enum Link {
    Archive(PathBuf),
    System(String),
    Framework(String),
    Search(PathBuf),
}

fn words(fragment: &str, windows: bool) -> Vec<String> {
    if !windows {
        return shlex::split(fragment).expect("Invalid CMake link fragment");
    }
    // CMake quotes Windows paths but their backslashes are literal, not shell
    // escapes. Quotes cannot occur inside a Windows file name.
    let mut quoted = false;
    let mut word = String::new();
    let mut result = Vec::new();
    for ch in fragment.chars() {
        if ch == '"' {
            quoted = !quoted;
        } else if ch.is_whitespace() && !quoted {
            if !word.is_empty() {
                result.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    assert!(!quoted, "Unclosed CMake path quote");
    if !word.is_empty() {
        result.push(word);
    }
    result
}

fn parse(tokens: &[String], build: &Path, windows: bool) -> Vec<Link> {
    let mut result = Vec::new();
    let mut tokens = tokens.iter();
    while let Some(token) = tokens.next() {
        let extension = Path::new(token)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if token == "-framework" {
            result.push(Link::Framework(
                tokens.next().expect("Missing framework").clone(),
            ));
        } else if let Some(name) = token.strip_prefix("-l") {
            result.push(Link::System(name.into()));
        } else if let Some(path) = token
            .strip_prefix("-L")
            .or_else(|| token.strip_prefix("/LIBPATH:"))
        {
            result.push(Link::Search(path.into()));
        } else if extension.eq_ignore_ascii_case("a") || extension.eq_ignore_ascii_case("lib") {
            let path = PathBuf::from(token);
            if windows && path.components().count() == 1 && !build.join(&path).is_file() {
                // Bare Windows SDK library names are resolved by the toolchain.
                result.push(Link::System(
                    path.file_stem().unwrap().to_str().unwrap().into(),
                ));
            } else {
                result.push(Link::Archive(if path.is_absolute() {
                    path
                } else {
                    build.join(path)
                }));
            }
        } else if extension.eq_ignore_ascii_case("tbd") && token.contains(".sdk/usr/lib/") {
            let name = Path::new(token).file_stem().unwrap().to_str().unwrap();
            result.push(Link::System(name.trim_start_matches("lib").into()));
        } else if !windows
            && token
                .strip_prefix("-Wl,-rpath,")
                .is_some_and(|path| path.ends_with(".sdk/usr/lib"))
        {
            // Corrosion's Rust system-library probe can add an SDK rpath.
            // Static archives/system libraries need no runtime SDK lookup.
        } else {
            panic!(
                "Unexpected native link input (only static archives and system libraries are supported): {token}"
            );
        }
    }
    result
}

fn coff_import_dll(member: &[u8]) -> Option<&str> {
    // IMPORT_OBJECT_HEADER is 20 bytes, followed by NUL-terminated symbol
    // and DLL names. SizeOfData excludes the header and archive padding.
    let size = u32::from_le_bytes(member.get(12..16)?.try_into().ok()?);
    let data = member.get(20..)?;
    if data.len() != usize::try_from(size).ok()? {
        return None;
    }
    let symbol_end = data.iter().position(|&byte| byte == 0)?;
    let name = data.get(symbol_end + 1..)?;
    let name_end = name.iter().position(|&byte| byte == 0)?;
    std::str::from_utf8(&name[..name_end]).ok()
}

fn verify_archive(path: &Path) {
    let bytes =
        fs::read(path).unwrap_or_else(|error| panic!("Cannot read {}: {error}", path.display()));
    assert!(
        bytes.starts_with(b"!<arch>\n"),
        "Not a static archive: {}",
        path.display()
    );
    let mut position = 8;
    while position < bytes.len() {
        let header = &bytes[position..position + 60];
        let length: usize = std::str::from_utf8(&header[48..58])
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let member = &bytes[position + 60..position + 60 + length];
        let name = std::str::from_utf8(&header[..16]).unwrap().trim();
        // COFF short import objects: Sig1=0, Sig2=0xffff, Version=0.
        // Bigobj uses Version=2 and remains a valid static object.
        if !matches!(name, "/" | "//" | "/SYM64/") && member.starts_with(&[0, 0, 255, 255, 0, 0]) {
            let dll = coff_import_dll(member)
                .unwrap_or_else(|| panic!("Malformed COFF import object: {}", path.display()));
            // Rust 1.98's std bundles these raw-dylib imports even with
            // crt-static. Permit the exact system DLLs, not arbitrary import
            // libraries or dynamic CRTs. Packaging also audits the final EXE.
            assert!(
                ["bcryptprimitives.dll", "api-ms-win-core-synch-l1-2-0.dll"]
                    .iter()
                    .any(|system| dll.eq_ignore_ascii_case(system)),
                "DLL import library is not a static dependency: {} ({dll})",
                path.display()
            );
        }
        position += 60 + length + length % 2;
    }
}

/// Present only static archives to linkers which otherwise prefer a stale
/// sibling dylib in a shared `CMake` cache. Symlinks do not duplicate the cache.
#[cfg(unix)]
fn archive_search_path(build: &Path, path: &Path) -> PathBuf {
    let directory = build.join("static-link");
    fs::create_dir_all(&directory).unwrap();
    let link = directory.join(path.file_name().unwrap());
    let source = path.canonicalize().unwrap();
    match fs::read_link(&link) {
        Ok(previous) if previous == source => return directory,
        Ok(_) => fs::remove_file(&link).unwrap(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("Cannot stage static archive {}: {error}", link.display()),
    }
    std::os::unix::fs::symlink(source, link).unwrap();
    directory
}

#[cfg(not(unix))]
fn archive_search_path(_build: &Path, path: &Path) -> PathBuf {
    path.parent().unwrap().to_path_buf()
}

pub fn emit(build: &Path, windows: bool) {
    let reply = build.join(".cmake/api/v1/reply");
    let index = fs::read_dir(&reply)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("index-")
        })
        .max()
        .expect("CMake did not return a File API index");
    let read = |path: &Path| -> Value { serde_json::from_slice(&fs::read(path).unwrap()).unwrap() };
    let index = read(&index);
    let model = read(&reply.join(index["reply"]["codemodel-v2"]["jsonFile"].as_str().unwrap()));
    let target = model["configurations"][0]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["name"] == "macinrender_link_probe")
        .expect("Missing native link target");
    let target = read(&reply.join(target["jsonFile"].as_str().unwrap()));
    let mut tokens = Vec::new();
    for fragment in target["link"]["commandFragments"].as_array().unwrap() {
        match fragment["role"].as_str().unwrap() {
            "libraries" | "libraryPath" => {
                tokens.extend(words(fragment["fragment"].as_str().unwrap(), windows));
            }
            "flags" => {} // CMake's executable subsystem/architecture flags belong to its probe.
            role => panic!("Unexpected CMake link role: {role}"),
        }
    }
    let links = parse(&tokens, build, windows);
    assert!(
        links.iter().any(|link| matches!(link, Link::Archive(_))),
        "No native static libraries"
    );
    let mut checked = std::collections::HashSet::new();
    for link in links {
        match link {
            Link::Archive(path) => {
                if checked.insert(path.clone()) {
                    verify_archive(&path);
                }
                println!(
                    "cargo:rustc-link-search=native={}",
                    archive_search_path(build, &path).display()
                );
                let stem = path.file_stem().unwrap().to_str().unwrap();
                let name = if windows {
                    stem
                } else {
                    stem.strip_prefix("lib").unwrap_or(stem)
                };
                println!("cargo:rustc-link-lib=static={name}");
            }
            Link::System(name) => println!("cargo:rustc-link-lib=dylib={name}"),
            Link::Framework(name) => println!("cargo:rustc-link-lib=framework={name}"),
            Link::Search(path) => println!("cargo:rustc-link-search=native={}", path.display()),
        }
    }
    if !windows {
        println!("cargo:rustc-link-lib=dylib=c++");
    }
    fs::write(
        build.join("native-link.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "linkage": "static", "link_inputs": tokens,
        }))
        .unwrap(),
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_object(dll: &str) -> Vec<u8> {
        let data = format!("symbol\0{dll}\0");
        let mut member = vec![0; 20];
        member[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        member[6..8].copy_from_slice(&0x8664_u16.to_le_bytes());
        member[12..16].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        member[18..20].copy_from_slice(&4_u16.to_le_bytes()); // IMPORT_OBJECT_NAME
        member.extend_from_slice(data.as_bytes());
        member
    }

    fn archive(members: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let mut bytes = b"!<arch>\n".to_vec();
        for (name, member) in members {
            let header = format!(
                "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                0,
                0,
                0,
                "100644",
                member.len()
            );
            assert_eq!(header.len(), 60);
            bytes.extend_from_slice(header.as_bytes());
            bytes.extend_from_slice(member);
            if member.len() % 2 != 0 {
                bytes.push(b'\n');
            }
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), bytes).unwrap();
        file
    }

    #[test]
    fn rust_static_archives_accept_only_the_known_system_dll_imports() {
        let random = import_object("BCRYPTPRIMITIVES.dll");
        let sync = import_object("api-ms-win-core-synch-l1-2-0.dll");
        let file = archive(&[
            ("/", &[0, 0, 255, 255, 0, 0]), // archive index, not an object
            ("code.obj/", &[0x64, 0x86, 0, 0]),
            ("big.obj/", &[0, 0, 255, 255, 2, 0]),
            ("/123", &random), // long archive name used by Rust's imports
            ("/456", &sync),
        ]);
        verify_archive(file.path());
    }

    #[test]
    fn rust_static_archives_still_reject_non_system_and_dynamic_crt_imports() {
        for dll in [
            "mradm_capi.dll",
            "libopenblas.dll",
            "VCRUNTIME140.dll",
            "api-ms-win-crt-runtime-l1-1-0.dll",
            "bcryptprimitives.dll.evil",
            r"C:\custom\bcryptprimitives.dll",
        ] {
            let allowed = import_object("bcryptprimitives.dll");
            let rejected = import_object(dll);
            let file = archive(&[("/123", &allowed), ("/456", &rejected)]);
            assert!(
                std::panic::catch_unwind(|| verify_archive(file.path())).is_err(),
                "accepted {dll}"
            );
        }
    }

    #[test]
    fn malformed_short_import_objects_are_rejected() {
        let valid = import_object("bcryptprimitives.dll");
        let mut unterminated = valid[..valid.len() - 1].to_vec();
        let size = u32::try_from(unterminated.len() - 20).unwrap();
        unterminated[12..16].copy_from_slice(&size.to_le_bytes());
        for member in [&valid[..6], &valid[..valid.len() - 1], &unterminated] {
            let file = archive(&[("/123", member)]);
            assert!(std::panic::catch_unwind(|| verify_archive(file.path())).is_err());
        }
    }

    #[test]
    fn paths_and_transitive_link_order_are_preserved() {
        let args = words(
            r#""sub dir/libapi.a" libbackend.a -framework Accelerate -lm"#,
            false,
        );
        assert_eq!(
            parse(&args, Path::new("/build"), false),
            vec![
                Link::Archive("/build/sub dir/libapi.a".into()),
                Link::Archive("/build/libbackend.a".into()),
                Link::Framework("Accelerate".into()),
                Link::System("m".into()),
            ]
        );
        assert_eq!(
            words(r#""C:\Program Files\native.lib" kernel32.lib"#, true),
            vec![r"C:\Program Files\native.lib", "kernel32.lib"]
        );
    }

    #[test]
    #[should_panic(expected = "Unexpected native link input")]
    fn dynamic_native_libraries_are_rejected() {
        parse(
            &["/opt/homebrew/lib/libnative.dylib".into()],
            Path::new("/build"),
            false,
        );
    }

    #[test]
    fn rust_system_library_probe_does_not_embed_an_sdk_rpath() {
        let tokens = words(
            "-Wl,-rpath,/Developer/SDKs/MacOSX.sdk/usr/lib -lSystem -liconv",
            false,
        );
        assert_eq!(
            parse(&tokens, Path::new("/build"), false),
            [Link::System("System".into()), Link::System("iconv".into())]
        );
    }
}

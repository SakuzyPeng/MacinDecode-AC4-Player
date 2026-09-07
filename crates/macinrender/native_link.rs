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
        } else {
            panic!(
                "Unexpected native link input (only static archives and system libraries are supported): {token}"
            );
        }
    }
    result
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
        assert!(
            matches!(name, "/" | "//" | "/SYM64/") || !member.starts_with(&[0, 0, 255, 255, 0, 0]),
            "DLL import library is not a static dependency: {}",
            path.display()
        );
        position += 60 + length + length % 2;
    }
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
                    path.parent().unwrap().display()
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
}

#![forbid(unsafe_code)]
//! Build-script helper: stage only this pinned build's native libraries.
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

fn hash(path: &Path) -> Vec<u8> {
    Sha256::digest(fs::read(path).expect("read staged image library")).to_vec()
}

/// Called from each native binary's build.rs with `pilotdeck-image-native` as a
/// direct build-dependency. Both executable and libtest search relative paths.
pub fn stage_and_link() {
    let prefix = PathBuf::from(
        env::var_os("DEP_PILOTDECK_IMAGE_NATIVE_PREFIX").expect("image native prefix"),
    );
    let profile = env::var("DEP_PILOTDECK_IMAGE_NATIVE_PROFILE").expect("image native profile");
    assert!(profile.len() == 64 && profile.bytes().all(|b| b.is_ascii_hexdigit()));
    let name = format!("pilotdeck-libs-{profile}");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let target = out.ancestors().nth(3).expect("Cargo target profile");
    let dest = target.join(&name);
    let temp = target.join(format!(".{name}-{}", std::process::id()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&temp)
        .expect("create private image staging directory");
    let libdir = prefix
        .join("lib")
        .canonicalize()
        .expect("image library directory");
    let mut files = Vec::new();
    for entry in fs::read_dir(&libdir).expect("read image libraries") {
        let entry = entry.expect("read image library entry");
        let file_name = entry.file_name();
        let n = file_name.to_str().expect("library filename UTF-8");
        if !(n.contains(".so") || n.ends_with(".dylib")) {
            continue;
        }
        assert!(
            n.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        );
        let source = entry.path().canonicalize().expect("resolve native library");
        assert!(
            source.starts_with(&libdir) && source.is_file(),
            "invalid native library"
        );
        fs::copy(source, temp.join(&file_name)).expect("stage image library");
        files.push(file_name);
    }
    assert!(!files.is_empty(), "no image libraries built");
    if fs::rename(&temp, &dest).is_err() {
        assert!(dest.is_dir(), "native library destination unavailable");
        let actual: std::collections::BTreeSet<_> = fs::read_dir(&dest)
            .expect("read staged image libraries")
            .map(|e| e.expect("library entry").file_name())
            .collect();
        let expected: std::collections::BTreeSet<_> = files.iter().cloned().collect();
        assert_eq!(actual, expected, "staged image library inventory mismatch");
        for f in &files {
            assert_eq!(
                hash(&dest.join(f)),
                hash(&temp.join(f)),
                "staged image library mismatch"
            );
        }
        fs::remove_dir_all(&temp).expect("remove own staging directory");
    }
    // The source installer reads this marker from the *new* build target;
    // it must not assume the updater's own previously compiled profile.
    let marker = target.join("pilotdeck-image-profile");
    let marker_tmp = target.join(format!(".pilotdeck-image-profile-{}", std::process::id()));
    fs::write(&marker_tmp, format!("{name}\n")).expect("write image profile marker");
    fs::rename(marker_tmp, marker).expect("publish image profile marker");
    let os = env::var("CARGO_CFG_TARGET_OS").expect("target OS");
    match os.as_str() {
        "linux" => {
            println!(
                "cargo:rustc-link-arg=-Wl,-rpath-link,{}/lib",
                prefix.display()
            );
            println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags,-rpath,$ORIGIN/{name}");
            println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags,-rpath,$ORIGIN/../{name}");
        }
        "macos" => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/{name}");
            println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../{name}");
        }
        _ => panic!("image native libraries unsupported on {os}"),
    }
    println!("cargo:rustc-env=PDRS_IMAGE_LIBRARY_DIR={name}");
}

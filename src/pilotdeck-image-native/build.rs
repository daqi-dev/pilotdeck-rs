use sha2::{Digest, Sha256};
use std::{env, fs, os::unix::fs::DirBuilderExt, path::PathBuf, process::Command};

fn main() {
    assert_eq!(
        env::var("HOST").expect("HOST"),
        env::var("TARGET").expect("TARGET"),
        "cross compilation of image codecs is not supported"
    );
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    // Never accept an alternate source directory from the process environment.
    // Copy checked bytes to a private per-attempt snapshot before invoking tar;
    // a later replacement of the checkout cannot change what gets compiled.
    let vendor = manifest.parent().expect("workspace root").join("vendor");
    let source = vendor.join("image-codecs-sources");
    for dir in [&vendor, &source] {
        let ty = fs::symlink_metadata(dir)
            .expect("source directory")
            .file_type();
        assert!(
            ty.is_dir() && !ty.is_symlink(),
            "source directory must be a real directory"
        );
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let snapshot = (0..1000)
        .find_map(|n| {
            let path = out.join(format!("image-source-snapshot-{}-{n}", std::process::id()));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => Some(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => panic!("create source snapshot: {error}"),
            }
        })
        .expect("source snapshot name exhaustion");
    let pins = include_str!("source-hashes.txt");
    let expected_names: std::collections::BTreeSet<_> = pins
        .lines()
        .map(|line| {
            line.split_once("  ")
                .expect("invalid source pin")
                .1
                .to_owned()
        })
        .collect();
    let actual_names: std::collections::BTreeSet<_> = fs::read_dir(&source)
        .expect("source inventory")
        .map(|entry| {
            entry
                .expect("source entry")
                .file_name()
                .into_string()
                .expect("source name UTF-8")
        })
        .collect();
    assert_eq!(
        actual_names, expected_names,
        "source archive inventory mismatch"
    );
    for line in pins.lines() {
        let (expected, name) = line.split_once("  ").expect("invalid source pin");
        assert!(
            !name.contains('/')
                && !name.contains('\\')
                && (name.ends_with(".tar.gz")
                    || name.ends_with(".tar.xz")
                    || name.ends_with(".tar.bz2")),
            "invalid source name"
        );
        let path = source.join(name);
        let ty = fs::symlink_metadata(&path)
            .expect("source archive")
            .file_type();
        assert!(
            ty.is_file() && !ty.is_symlink(),
            "source archive must be a regular file"
        );
        let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let actual = format!("{:x}", Sha256::digest(&bytes));
        assert_eq!(
            actual,
            expected,
            "source hash mismatch for {}",
            path.display()
        );
        fs::write(snapshot.join(name), bytes).expect("write verified source snapshot");
        println!("cargo:rerun-if-changed={}", path.display());
    }
    for path in [
        "source-build.sh",
        "strip-vips-extras.py",
        "fix-macos-install-names.py",
        "source-hashes.txt",
    ] {
        println!("cargo:rerun-if-changed={}", manifest.join(path).display());
    }
    let status = Command::new("sh")
        .arg(manifest.join("source-build.sh"))
        .env("PDRS_IMAGE_SOURCE_DIR", &snapshot)
        .status()
        .expect("image source build start");
    assert!(status.success(), "image source build failed: {status}");
    let prefix = PathBuf::from(
        fs::read_to_string(out.join("image-native-prefix"))
            .expect("image native prefix marker")
            .trim(),
    );
    assert!(
        prefix.starts_with(&out) && prefix.join("lib").is_dir(),
        "invalid image native prefix"
    );
    println!("cargo:prefix={}", prefix.display());
    let mut identity = Sha256::new();
    for path in [
        "source-hashes.txt",
        "source-build.sh",
        "strip-vips-extras.py",
        "fix-macos-install-names.py",
    ] {
        identity.update(fs::read(manifest.join(path)).expect("read image build identity"));
    }
    // Include the actual emitted runtime bytes and filenames. Different OS,
    // architecture, compiler or build options cannot silently reuse an older
    // staging directory even when the pinned input archives are unchanged.
    identity.update(env::var("HOST").expect("HOST").as_bytes());
    identity.update(env::var("TARGET").expect("TARGET").as_bytes());
    let libdir = prefix
        .join("lib")
        .canonicalize()
        .expect("native library directory");
    let mut libraries = Vec::new();
    for entry in fs::read_dir(&libdir).expect("native library inventory") {
        let entry = entry.expect("native library entry");
        let name = entry
            .file_name()
            .into_string()
            .expect("library filename UTF-8");
        if !(name.contains(".so") || name.ends_with(".dylib")) {
            continue;
        }
        let file = entry.path().canonicalize().expect("resolve native library");
        assert!(
            file.starts_with(&libdir) && file.is_file(),
            "invalid native library"
        );
        libraries.push((name, file));
    }
    libraries.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!libraries.is_empty(), "empty native library inventory");
    for (name, file) in libraries {
        identity.update((name.len() as u64).to_be_bytes());
        identity.update(name.as_bytes());
        identity.update(Sha256::digest(fs::read(file).expect("read native library")));
    }
    println!("cargo:profile={:x}", identity.finalize());
}

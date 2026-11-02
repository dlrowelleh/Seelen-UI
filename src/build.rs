use std::{env, fs::create_dir, path::PathBuf};

use slu_utils::{checksums::CheckSums, signature::sign_minisign};

fn main() {
    println!("cargo:rerun-if-env-changed=SEELEN_ALLOW_UNSIGNED_BUILD");

    let _ = create_dir("gen");

    let mut checksums = CheckSums::new();
    read_folder_recursive(PathBuf::from("static"), &mut |path| {
        checksums.add(&path).unwrap();
    });

    let target_dir = target_dir();
    let sums_path = target_dir.join("SHA256SUMS");
    checksums.write(&sums_path).unwrap();

    if !cfg!(debug_assertions) {
        if allow_unsigned_build() {
            // Deliberately unsigned CI artifacts may be built without the repository's
            // Minisign private key. The runtime still validates SHA-256 checksums; only
            // the publisher-signature check is disabled in this explicitly opted-in build.
            println!("cargo:rustc-env=SEELEN_UNSIGNED_BUILD=1");
            println!("cargo:warning=Building unsigned: Minisign signature verification is disabled");
            std::fs::write(
                sums_path.with_extension("sig"),
                "UNSIGNED BUILD: SHA256SUMS publisher signature intentionally omitted",
            )
            .expect("Failed to write unsigned build marker");
        } else {
            println!("cargo:rustc-env=SEELEN_UNSIGNED_BUILD=0");
            sign_sha256sums(&sums_path);
        }
    } else {
        std::fs::write(
            sums_path.with_extension("sig"),
            "NOT SIGNED NEEDED FOR DEBUG",
        )
        .unwrap();
    }

    tauri_build::build();
}

fn allow_unsigned_build() -> bool {
    env::var("SEELEN_ALLOW_UNSIGNED_BUILD")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn target_dir() -> PathBuf {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // see <https://github.com/rust-lang/cargo/issues/5457>
    out_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read_folder_recursive<F>(path: PathBuf, cb: &mut F)
where
    F: FnMut(PathBuf),
{
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            read_folder_recursive(path, cb);
        } else {
            cb(path);
        }
    }
}

fn sign_sha256sums(path: &PathBuf) {
    let key_base64 =
        std::env::var("TAURI_SIGNING_PRIVATE_KEY").expect("TAURI_SIGNING_PRIVATE_KEY missing");
    let password = std::env::var("TAURI_SIGNING_PRIVATE_KEY_PASSWORD")
        .expect("TAURI_SIGNING_PRIVATE_KEY_PASSWORD missing");

    let data = std::fs::read(path).expect("Failed to read SHA256SUMS file");
    let signature = sign_minisign(&data, &key_base64, password).expect("Failed to sign data");

    let sig_path = path.with_extension("sig");
    std::fs::write(&sig_path, signature).expect("Failed to write signature");
}

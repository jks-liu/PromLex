use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

// Tauri dependency build scripts cache absolute paths. Cargo may reuse those
// scripts after the workspace moves, including JSON lists containing old paths.
pub fn relocate_tauri_metadata() -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=CARGO_MANIFEST_DIR");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    let build_dir = out_dir
        .parent()
        .and_then(Path::parent)
        .expect("Cargo build directory");
    for (key, value) in env::vars_os() {
        let key_text = key.to_string_lossy();
        if !key_text.starts_with("DEP_") {
            continue;
        }
        if key_text.ends_with("_PERMISSION_FILES_PATH") {
            let original = PathBuf::from(value);
            let destination = out_dir.join(format!("{key_text}.json"));
            let relocated = relocate_permission_list(&original, build_dir, &destination)?;
            if relocated != original {
                env::set_var(&key, relocated);
            }
        } else if key_text.ends_with("_GLOBAL_SCOPE_SCHEMA_PATH") {
            let original = PathBuf::from(value);
            let relocated = relocate_path(&original, build_dir);
            if relocated != original {
                env::set_var(&key, relocated);
            }
        }
    }
    Ok(())
}

fn relocate_permission_list(
    original: &Path,
    build_dir: &Path,
    destination: &Path,
) -> io::Result<PathBuf> {
    let list = relocate_path(original, build_dir);
    let paths: Vec<PathBuf> = serde_json::from_slice(&fs::read(&list)?)?;
    let relocated: Vec<PathBuf> = paths
        .iter()
        .map(|path| relocate_path(path, build_dir))
        .collect();
    if list != original || paths != relocated {
        fs::write(destination, serde_json::to_vec(&relocated)?)?;
        Ok(destination.to_path_buf())
    } else {
        Ok(original.to_path_buf())
    }
}

fn relocate_path(path: &Path, build_dir: &Path) -> PathBuf {
    let components: Vec<_> = path.components().collect();
    // Match the dependency's exact build hash; never guess another version.
    for index in (0..components.len().saturating_sub(2)).rev() {
        if components[index].as_os_str() == "build" && components[index + 2].as_os_str() == "out" {
            let suffix: PathBuf = components[index + 1..].iter().collect();
            let candidate = build_dir.join(suffix);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocates_only_existing_matching_dependency_outputs() {
        let root = env::temp_dir().join(format!("promlex-relocation-{}", std::process::id()));
        let build = root.join("new workspace/target/debug/build");
        let file = build.join("tauri-abc/out/permissions/app.toml");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "permission").unwrap();
        let old = root.join("old workspace/target/debug/build/tauri-abc/out/permissions/app.toml");
        assert_eq!(relocate_path(&old, &build), file);
        let missing = root.join("old/target/debug/build/tauri-other/out/permissions/app.toml");
        assert_eq!(relocate_path(&missing, &build), missing);
        let external = root.join("registry/plugin/permissions/default.toml");
        assert_eq!(relocate_path(&external, &build), external);
        let list = build.join("tauri-abc/out/tauri-permission-files");
        fs::write(
            &list,
            serde_json::to_vec(&vec![old, external.clone()]).unwrap(),
        )
        .unwrap();
        let old_list =
            root.join("old workspace/target/debug/build/tauri-abc/out/tauri-permission-files");
        let destination = root.join("relocated-permissions.json");
        assert_eq!(
            relocate_permission_list(&old_list, &build, &destination).unwrap(),
            destination
        );
        let relocated: Vec<PathBuf> =
            serde_json::from_slice(&fs::read(&destination).unwrap()).unwrap();
        assert_eq!(relocated, vec![file.clone(), external.clone()]);
        fs::write(&list, serde_json::to_vec(&vec![file, external]).unwrap()).unwrap();
        assert_eq!(
            relocate_permission_list(&list, &build, &destination).unwrap(),
            list
        );
        fs::remove_dir_all(root).unwrap();
    }
}

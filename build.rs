use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const FRONTEND_FILES: &[&str] = &[
    "index.html",
    "package.json",
    "package-lock.json",
    "tsconfig.json",
    "vite.config.ts",
];
const FRONTEND_ASSETS: &[&str] = &["index.html", "assets/app.js", "assets/pdf.worker.mjs"];

fn main() {
    for input in [
        "web/index.html",
        "web/package.json",
        "web/package-lock.json",
        "web/tsconfig.json",
        "web/vite.config.ts",
        "web/src",
    ] {
        println!("cargo:rerun-if-changed={input}");
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let build_workspace = out_dir.join("frontend-build");
    if build_workspace.exists() {
        fs::remove_dir_all(&build_workspace)
            .expect("failed to clear isolated frontend build directory");
    }
    fs::create_dir_all(&build_workspace).expect("failed to create frontend build directory");

    let checked_frontend = Path::new("web");
    for relative_path in FRONTEND_FILES {
        fs::copy(
            checked_frontend.join(relative_path),
            build_workspace.join(relative_path),
        )
        .unwrap_or_else(|error| panic!("failed to copy web/{relative_path}: {error}"));
    }
    copy_directory(&checked_frontend.join("src"), &build_workspace.join("src"));

    run_npm(&build_workspace, &["ci"]);
    run_npm(&build_workspace, &["run", "build"]);

    let dist = build_workspace.join("dist");
    let staged_assets = out_dir.join("web");
    if staged_assets.exists() {
        fs::remove_dir_all(&staged_assets).expect("failed to clear staged frontend assets");
    }
    for asset in FRONTEND_ASSETS {
        let source = dist.join(asset);
        let destination = staged_assets.join(asset);
        fs::create_dir_all(destination.parent().expect("frontend asset has a parent"))
            .expect("failed to create staged frontend directory");
        fs::copy(&source, &destination).unwrap_or_else(|error| {
            panic!("failed to stage {}: {error}", source.display());
        });
    }
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination)
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", destination.display()));
    for entry in fs::read_dir(source)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source.display()))
    {
        let entry = entry.expect("failed to read frontend source entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_directory(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).unwrap_or_else(|error| {
                panic!(
                    "failed to copy {} to {}: {error}",
                    source_path.display(),
                    destination_path.display()
                )
            });
        }
    }
}

fn run_npm(directory: &Path, arguments: &[&str]) {
    let status = Command::new("npm")
        .args(arguments)
        .current_dir(directory)
        .status()
        .unwrap_or_else(|error| panic!("failed to run npm {}: {error}", arguments.join(" ")));
    assert!(status.success(), "npm {} failed", arguments.join(" "));
}

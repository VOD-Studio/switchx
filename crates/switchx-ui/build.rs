fn main() {
    // A cached build script may be reused from another checkout.
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory");
    let project_dir = std::path::PathBuf::from(manifest_dir).join("../..");
    for asset in ["assets/app-icon.png", "assets/logo.png"] {
        let path = project_dir.join(asset);
        println!("cargo:rerun-if-changed={}", path.display());
        let data = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("cannot read UI image {asset}: {error}"));
        assert!(
            !data.starts_with(b"version https://git-lfs.github.com/spec/v1"),
            "UI image {asset} is a Git LFS pointer. Run `git lfs install` and `git lfs pull` \
             from the SwitchX repository root, then rebuild."
        );
        assert!(
            data.starts_with(b"\x89PNG\r\n\x1a\n"),
            "UI image {asset} has an invalid PNG signature. Restore the image and rebuild."
        );
    }
    let app = project_dir.join("ui/app.slint");
    slint_build::compile(app).expect("compile Slint UI");
}

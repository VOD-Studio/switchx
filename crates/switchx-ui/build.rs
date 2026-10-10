fn main() {
    // A cached build script may be reused from another checkout.
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory");
    let app = std::path::PathBuf::from(manifest_dir).join("../../ui/app.slint");
    slint_build::compile(app).expect("compile Slint UI");
}

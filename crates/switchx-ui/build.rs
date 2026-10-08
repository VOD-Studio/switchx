fn main() {
    let app = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/app.slint");
    slint_build::compile(app).expect("compile Slint UI");
}

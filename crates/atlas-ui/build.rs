fn main() {
    // Compile the Slint UI definition. This lives in atlas-ui rather than
    // atlas-app so that AppShell (and any future atlas-ui consumers) can
    // reference the generated types directly without a circular dependency.
    //
    // atlas-app/build.rs is a no-op; atlas-app uses re-exported types from
    // this crate. Future: if a standalone demo binary needs the same UI,
    // extract into a dedicated `atlas-slint` crate (Option B in the original
    // design doc).
    let ui_file = "../../assets/ui/atlas.slint";
    println!("cargo:rerun-if-changed={ui_file}");
    println!("cargo:rerun-if-changed=../../assets/ui/theme.slint");
    println!("cargo:rerun-if-changed=../../assets/ui/components");
    println!("cargo:rerun-if-changed=../../assets/ui/views");
    slint_build::compile(ui_file).expect("failed to compile Slint UI");
}

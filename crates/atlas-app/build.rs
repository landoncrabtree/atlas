fn main() {
    let ui_file = "../../assets/ui/atlas.slint";
    println!("cargo:rerun-if-changed={ui_file}");
    slint_build::compile(ui_file).expect("failed to compile Slint UI");
}

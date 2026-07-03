fn main() {
    // Slint compilation is handled by atlas-ui/build.rs.
    // Keep this build script limited to atlas-app packaging metadata.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/branding/atlas.ico");
        res.compile()
            .expect("failed to embed Atlas Windows icon resource");
    }
}

fn main() {
    // Embed the icon into the executable on Windows so Explorer and the
    // taskbar show it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
        resource.set_icon(&format!("{manifest_dir}\\..\\..\\assets\\rustmic.ico"));
        resource.compile().expect("could not compile the Windows resources");
    }
}

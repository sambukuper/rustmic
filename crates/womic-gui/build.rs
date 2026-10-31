fn main() {
    // Embed the icon into the executable on Windows so Explorer and the
    // taskbar show it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
        let icon = format!("{manifest_dir}\\..\\..\\assets\\rustmic.ico");
        println!("cargo:rerun-if-changed={icon}");
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(&icon);
        resource.compile().expect("could not compile the Windows resources");
    }
}

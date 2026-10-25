fn main() {
    println!("cargo:rerun-if-changed=assets/vdisplay.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/vdisplay.ico")
            .set("ProductName", "Vdisplay")
            .set("FileDescription", "Vdisplay case screen controller")
            .set("CompanyName", "visiuun");
        if let Err(e) = res.compile() {
            // Missing Windows SDK resource compiler: build without the embedded icon.
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}

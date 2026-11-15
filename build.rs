fn main() {
    println!("cargo:rerun-if-changed=assets/securechat.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/securechat.ico");
        res.set("ProductName", "SecureChat");
        res.set("FileDescription", "SecureChat - LAN end-to-end encrypted messenger");
        res.set("CompanyName", "SecureChat");
        res.set("LegalCopyright", "MIT License");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon resource not embedded: {e}");
        }
    }
}

//! Embeds the Windows icon and version resource into the executable, so the
//! released binary looks like an application in Explorer and the taskbar
//! rather than an anonymous console exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/spot.ico");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/spot.ico");
    res.set("ProductName", "spot");
    res.set(
        "FileDescription",
        "A standalone Spotify player for the terminal",
    );
    // The version fields come from CARGO_PKG_VERSION automatically.

    // Compiling resources needs rc.exe from the Windows SDK. It is present on
    // GitHub's windows runners and alongside any MSVC toolchain, but a machine
    // without it should still get a working binary — just an unbranded one.
    if let Err(e) = res.compile() {
        println!("cargo:warning=could not embed the Windows resource: {e}");
    }
}

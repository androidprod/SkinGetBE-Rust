/// Build script for SkinGetBE
/// Handles Windows resource compilation and icon embedding

fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();

        // Set icon if it exists
        if std::path::Path::new("res/app.ico").exists() {
            res.set_icon("res/app.ico");
        } else if std::path::Path::new("app.ico").exists() {
            res.set_icon("app.ico");
        }

        // Set version information (note: winres uses u64 format)
        // Version format is: (major << 48) | (minor << 32) | (patch << 16) | build
        res.set_version_info(
            winres::VersionInfo::PRODUCTVERSION,
            0x0001000000000000, // 0.1.0.0
        );

        res.set_version_info(
            winres::VersionInfo::FILEVERSION,
            0x0001000000000000, // 0.1.0.0
        );

        // Set string properties
        res.set("ProductName", "SkinGetBE");
        res.set("FileDescription", "Minecraft Bedrock Edition Skin Extraction Tool");
        res.set("CompanyName", "SkinGetBE Contributors");
        res.set("ProductVersion", "0.1.0");
        res.set("FileVersion", "0.1.0");
        res.set("LegalCopyright", "MIT License");
        res.set("OriginalFilename", "skingetbe.exe");

        // Compile resource
        if let Err(e) = res.compile() {
            eprintln!("Warning: Failed to compile Windows resources: {}", e);
        }
    }
}

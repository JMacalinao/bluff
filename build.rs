//! Embeds an icon, version information and a manifest in bluff.exe. An exe
//! without them looks like malware to Windows Defender's heuristics.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=src/windows/bluff.ico");
    println!("cargo:rerun-if-changed=src/windows/bluff.manifest");

    // rc files take forward slashes, so Windows paths need no escaping.
    let dir = env::var("CARGO_MANIFEST_DIR").unwrap().replace('\\', "/") + "/src/windows";
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numeric = version.split(['.', '-']).take(3).collect::<Vec<_>>().join(",");
    let description = env::var("CARGO_PKG_DESCRIPTION").unwrap();
    let rc = format!(
        r#"1 ICON "{dir}/bluff.ico"
1 24 "{dir}/bluff.manifest"
1 VERSIONINFO
FILEVERSION {numeric},0
PRODUCTVERSION {numeric},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "JJ Macalinao"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "bluff"
      VALUE "LegalCopyright", "Copyright JJ Macalinao"
      VALUE "OriginalFilename", "bluff.exe"
      VALUE "ProductName", "bluff"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let (rc_path, object) = (out.join("bluff.rc"), out.join("bluff-resources.o"));
    fs::write(&rc_path, rc).unwrap();
    let windres = format!("{}-w64-mingw32-windres", env::var("CARGO_CFG_TARGET_ARCH").unwrap());
    let status = Command::new(&windres)
        .arg(&rc_path)
        .args(["-O", "coff", "-o"])
        .arg(&object)
        .status()
        .unwrap_or_else(|error| panic!("{windres} (from llvm-mingw) must be on PATH: {error}"));
    assert!(status.success(), "{windres} failed");
    println!("cargo:rustc-link-arg-bins={}", object.display());
}

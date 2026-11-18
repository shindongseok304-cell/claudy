fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.manifest");
    println!("cargo:rerun-if-changed=res/app.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // 실행 파일의 버전 정보를 Cargo.toml 의 version 과 맞춘다
        let ver = std::env::var("CARGO_PKG_VERSION").unwrap();
        let nums: Vec<&str> = ver.split('.').collect();
        let comma = format!("{},{},{},0", nums[0], nums.get(1).unwrap_or(&"0"), nums.get(2).unwrap_or(&"0"));
        let res = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res");
        let rc = std::fs::read_to_string(res.join("app.rc"))
            .unwrap()
            .replace("FILEVERSION 0,4,0,0", &format!("FILEVERSION {comma}"))
            .replace("PRODUCTVERSION 0,4,0,0", &format!("PRODUCTVERSION {comma}"))
            .replace("\"0.4.0\"", &format!("\"{ver}\""))
            .replace("\"app.ico\"", &format!("\"{}\"", res.join("app.ico").display().to_string().replace('\\', "/")))
            .replace("\"app.manifest\"", &format!("\"{}\"", res.join("app.manifest").display().to_string().replace('\\', "/")));
        let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("app.rc");
        std::fs::write(&out, rc).unwrap();
        embed_resource::compile_for(&out, ["ClaudyManager"], embed_resource::NONE).manifest_required().unwrap();
    }
}

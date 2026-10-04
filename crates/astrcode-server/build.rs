//! 让 cargo 追踪编译期内嵌的产物目录。
//!
//! `crates/astrcode-webui/www` 的根目录文件是源码，其中 `wasm/` 由 `scripts/build.sh`
//! 生成、不入库（见仓库根 `.gitignore`）。

use std::{env, path::PathBuf};

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo 会注入 CARGO_MANIFEST_DIR"));
    let web_ui_www = manifest_dir.join("../../crates/astrcode-webui/www");

    // 内嵌发生在 rustc 展开 derive 时，cargo 无从得知产物已变化，必须显式声明依赖；
    // `rust-embed` 自己不做这件事。目录的 mtime 只随直接子项变化，而 wasm 在 `www/wasm/`
    // 下，因此两级都要声明。
    for path in [&web_ui_www, &web_ui_www.join("wasm")] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

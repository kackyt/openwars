use std::env;
use std::fs;
use std::path::Path;

/// リポジトリ未追跡のまま参照されることがあるテストモジュール。
/// 存在する環境（作成者の作業ツリー）でだけcfgを立ててコンパイルし、
/// 存在しないクローンでもlibテスト全体がビルドできるようにする。
const OPTIONAL_TEST_MODULES: &[(&str, &str)] = &[(
    "src/ai/squad/planning_contract_tests.rs",
    "openwars_planning_contract_tests",
)];

fn main() {
    for (path, cfg) in OPTIONAL_TEST_MODULES {
        println!("cargo::rustc-check-cfg=cfg({cfg})");
        println!("cargo:rerun-if-changed={path}");
        if Path::new(path).exists() {
            println!("cargo:rustc-cfg={cfg}");
        }
    }

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("generated_maps.rs");

    // マップディレクトリのパス
    let map_dir = "src/resources/master_data/map";
    println!("cargo:rerun-if-changed={}", map_dir);

    let entries = fs::read_dir(map_dir).expect("Failed to read map directory");

    let mut generated_code = String::from("pub const MAPS: &[(&str, &str)] = &[\n");

    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("csv") {
            let file_name = path.file_stem().unwrap().to_str().unwrap();
            // include_str! のパスは、この generated_maps.rs が include! される場所からの相対パス、
            // あるいは絶対パスである必要がある。
            // ここでは src/resources/master_data.rs から include! することを想定し、
            // そこからの相対パス（"master_data/map/xxx.csv"）を生成する。
            // しかし、build.rs から見ると "src/resources/master_data/map/xxx.csv"。
            //
            // 確実なのは、CSVの中身をそのまま文字列リテラルとして埋め込むこと。
            let content = fs::read_to_string(&path).expect("Failed to read map file");
            generated_code.push_str(&format!("    ({:?}, {:?}),\n", file_name, content));
        }
    }

    generated_code.push_str("];\n");

    fs::write(&dest_path, generated_code).unwrap();
}

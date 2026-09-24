#[path = "msduck_pending_drain.rs"]
mod msduck_pending_drain;

use crate::{is_compiler, link_windows_system_libs, win_target, write_bindings};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[derive(serde::Deserialize)]
struct Sources {
    cpp_files: HashSet<String>,
    include_dirs: HashSet<String>,
}

#[derive(serde::Deserialize)]
struct Manifest {
    base: Sources,
    extensions: HashMap<String, Sources>,
}

fn add_extension(
    manifest: &Manifest,
    extension: &str,
    cpp_files: &mut HashSet<String>,
    include_dirs: &mut HashSet<String>,
) {
    let sources = manifest.extensions.get(extension).unwrap_or_else(|| {
        let mut available = manifest.extensions.keys().cloned().collect::<Vec<_>>();
        available.sort();
        panic!(
            "extension `{extension}` is missing from duckdb manifest; available extensions: {}",
            available.join(", ")
        );
    });
    cpp_files.extend(sources.cpp_files.clone());
    include_dirs.extend(sources.include_dirs.clone());
}

fn extension_enabled(extension: &str) -> bool {
    // Review build_bundled_cmake::enabled_extensions when changing this gate;
    // the backend mechanisms and supported extension sets intentionally differ.
    extension == "core_functions"
        || (extension == "parquet" && cfg!(feature = "parquet"))
        || (extension == "json" && cfg!(feature = "json"))
}

fn untar_archive(out_dir: &str) {
    let path = "duckdb.tar.gz";

    let tar_gz = std::fs::File::open(path).expect("archive file");
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);
    // These archives are generated for reproducibility, not source mtimes.
    // Skipping mtime restoration also avoids Windows SetFileTime failures on
    // filesystems such as FAT32, which cannot represent Unix epoch mtimes.
    archive.set_preserve_mtime(false);
    archive.unpack(out_dir).expect("archive");
}

pub fn main(out_dir: &str, out_path: &Path) {
    untar_archive(out_dir);
    msduck_pending_drain::apply(out_dir);
    // msduck: the C API promises one state pointer per row. Constant window
    // state vectors must be flattened before exposing their backing storage.
    let capi_path = Path::new(out_dir).join("duckdb/src/main/capi/aggregate_function-c.cpp");
    let mut capi = std::fs::read_to_string(&capi_path).expect("aggregate C API source");
    for (before, after) in [
        ("\tDataChunk chunk;\n", "\tstate.Flatten(count);\n\tDataChunk chunk;\n"),
        ("void CAPIAggregateCombine(Vector &state, Vector &combined, AggregateInputData &aggr_input_data, idx_t count) {\n", "void CAPIAggregateCombine(Vector &state, Vector &combined, AggregateInputData &aggr_input_data, idx_t count) {\n\tcombined.Flatten(count);\n"),
    ] {
        assert_eq!(capi.matches(before).count(), 1, "aggregate C API patch drift");
        capi = capi.replace(before, after);
    }
    std::fs::write(capi_path, capi).expect("patch aggregate C API state vectors");


    // Private noncycling identity sequences must preserve the last allocation,
    // not the next counter, through checkpoint/WAL reconstruction.
    let sequence_path = Path::new(out_dir).join("duckdb/src/catalog/catalog_entry/sequence_catalog_entry.cpp");
    let mut sequence = std::fs::read_to_string(&sequence_path).expect("sequence catalog source");
    for (before, after) in [
        (r#"#include "duckdb/common/operator/add.hpp""#, r#"#include "duckdb/common/operator/add.hpp"
#include <cstring>
#include <limits>"#),
        (r#"namespace duckdb {
"#, r#"namespace duckdb {
static int64_t MSDuckIdentityBits(uint64_t bits) {
    int64_t value;
    std::memcpy(&value, &bits, sizeof(value));
    return value;
}
"#),
        (r#"start_value(info.start_value), min_value(info.min_value), max_value(info.max_value), cycle(info.cycle) {
}"#, r#"start_value(info.start_value), min_value(info.min_value), max_value(info.max_value), cycle(info.cycle) {
    if (info.usage_count && !info.cycle && info.name.rfind("__msduck_identity_", 0) == 0) {
        last_value = MSDuckIdentityBits(uint64_t(info.start_value) - uint64_t(info.increment));
    }
}"#),
        (r#"	result = data.counter;
"#, r#"    const bool identity = !data.cycle && name.rfind("__msduck_identity_", 0) == 0;
    // After at least one allocation, a wrapped counter lies outside every
    // reachable non-overflowing next-counter value for this increment.
    if (identity && data.usage_count &&
        ((data.increment > 0 && data.counter < std::numeric_limits<int64_t>::min() + data.increment) ||
         (data.increment < 0 && data.counter > std::numeric_limits<int64_t>::max() + data.increment))) {
        throw SequenceException("nextval: reached %s value of sequence \"%s\" (%lld)",
            data.increment > 0 ? "maximum" : "minimum", name,
            data.increment > 0 ? data.max_value : data.min_value);
    }
    result = data.counter;
"#),
        (r#"	bool overflow = !TryAddOperator::Operation(data.counter, data.increment, data.counter);
"#, r#"	bool overflow = !TryAddOperator::Operation(data.counter, data.increment, data.counter);
    if (identity && overflow) {
        data.counter = MSDuckIdentityBits(uint64_t(result) + uint64_t(data.increment));
        overflow = false;
    }
"#),
        (r#"throw SequenceException("nextval: reached minimum value"#, r#"if (name.rfind("__msduck_identity_", 0) == 0) { data.counter = result; }
			throw SequenceException("nextval: reached minimum value"#),
        (r#"throw SequenceException("nextval: reached maximum value"#, r#"if (name.rfind("__msduck_identity_", 0) == 0) { data.counter = result; }
			throw SequenceException("nextval: reached maximum value"#),
        (r#"		data.counter = v_counter;
"#, r#"		data.counter = v_counter;
        if (!data.cycle && name.rfind("__msduck_identity_", 0) == 0) {
            data.last_value = MSDuckIdentityBits(uint64_t(v_counter) - uint64_t(data.increment));
        }
"#),
    ] {
        assert_eq!(sequence.matches(before).count(), 1, "identity sequence patch drift");
        sequence = sequence.replace(before, after);
    }
    std::fs::write(sequence_path, sequence).expect("patch private identity sequence state");

    // During initial WAL replay the session default database does not exist
    // yet. Default-expression binders already have the owning table's catalog.
    let catalog_path = Path::new(out_dir).join("duckdb/src/catalog/catalog.cpp");
    let catalog = std::fs::read_to_string(&catalog_path).expect("catalog source");
    let before = "const string &GetDefaultCatalog(CatalogEntryRetriever &retriever) {\n\treturn DatabaseManager::GetDefaultDatabase(retriever.GetContext());\n}";
    let after = "const string &GetDefaultCatalog(CatalogEntryRetriever &retriever) {\n    auto &entry = retriever.GetSearchPath().GetDefault();\n    if (!DatabaseManager::Get(retriever.GetContext()).HasDefaultDatabase() && !IsInvalidCatalog(entry.catalog)) {\n        return entry.catalog;\n    }\n\treturn DatabaseManager::GetDefaultDatabase(retriever.GetContext());\n}";
    assert_eq!(catalog.matches(before).count(), 1, "WAL default catalog patch drift");
    std::fs::write(catalog_path, catalog.replace(before, after)).expect("patch WAL default catalog");

    // A volatile filter belongs above a cross product: pushing it into one
    // input changes its number of evaluations (notably scalar catalog maps).
    let pushdown_path = Path::new(out_dir).join("duckdb/src/optimizer/pushdown/pushdown_cross_product.cpp");
    let pushdown = std::fs::read_to_string(&pushdown_path).expect("cross product pushdown source");
    let before = "unique_ptr<LogicalOperator> FilterPushdown::PushdownCrossProduct(unique_ptr<LogicalOperator> op) {\n";
    let after = "unique_ptr<LogicalOperator> FilterPushdown::PushdownCrossProduct(unique_ptr<LogicalOperator> op) {\n    for (auto &filter : filters) {\n        if (filter->filter->IsVolatile()) {\n            return FinishPushdown(std::move(op));\n        }\n    }\n";
    assert_eq!(pushdown.matches(before).count(), 1, "volatile filter patch drift");
    std::fs::write(pushdown_path, pushdown.replace(before, after)).expect("patch volatile cross product filter");

    let relations_path = Path::new(out_dir).join("duckdb/src/optimizer/join_order/relation_manager.cpp");
    let relations = std::fs::read_to_string(&relations_path).expect("join relation source");
    let before = "\t\tif (op->type == LogicalOperatorType::LOGICAL_FILTER) {\n";
    let after = "\t\tif (op->type == LogicalOperatorType::LOGICAL_FILTER) {\n            bool volatile_filter = false;\n            for (auto &expression : op->expressions) {\n                volatile_filter = volatile_filter || expression->IsVolatile();\n            }\n            if (volatile_filter) {\n                RelationStats child_stats;\n                auto child_optimizer = optimizer.CreateChildOptimizer();\n                op->children[0] = child_optimizer.Optimize(std::move(op->children[0]), &child_stats);\n                ModifyStatsIfLimit(limit_op.get(), child_stats);\n                AddRelation(input_op, parent, child_stats);\n                return true;\n            }\n";
    assert_eq!(relations.matches(before).count(), 1, "volatile join relation patch drift");
    std::fs::write(relations_path, relations.replace(before, after)).expect("preserve volatile join filter");

    let include_path = Path::new(out_dir).join("duckdb/src/include");
    write_bindings(&include_path, out_path);

    // Publish the include directory so downstream crates that compile
    // their own C/C++ code (e.g. extension shims that #include
    // "duckdb.hpp") can pick it up from `DEP_DUCKDB_INCLUDE` without
    // having to glob the target tree. Requires `links = "duckdb"` in
    // the package manifest.
    println!("cargo:include={}", include_path.display());

    let manifest_file = std::fs::File::open(format!("{out_dir}/duckdb/manifest.json")).expect("manifest file");
    let manifest: Manifest = serde_json::from_reader(manifest_file).expect("reading manifest file");

    let mut cpp_files = HashSet::new();
    let mut include_dirs = HashSet::new();
    let mut extensions = manifest
        .extensions
        .keys()
        .filter(|name| extension_enabled(name))
        .cloned()
        .collect::<Vec<String>>();
    extensions.sort();

    cpp_files.extend(manifest.base.cpp_files.clone());
    include_dirs.extend(manifest.base.include_dirs.iter().cloned());

    let mut cfg = cc::Build::new();

    for extension in &extensions {
        add_extension(&manifest, extension, &mut cpp_files, &mut include_dirs);
        cfg.define(
            &format!("DUCKDB_EXTENSION_{}_LINKED", extension.to_uppercase()),
            Some("1"),
        );
    }
    cfg.define("DUCKDB_EXTENSION_AUTOINSTALL_DEFAULT", "1");
    cfg.define("DUCKDB_EXTENSION_AUTOLOAD_DEFAULT", "1");

    println!("cargo:rerun-if-changed=duckdb.tar.gz");

    cfg.include("duckdb");
    cfg.includes(include_dirs.iter().map(|dir| format!("{out_dir}/duckdb/{dir}")));

    let mut cpp_files_vec: Vec<String> = cpp_files.into_iter().collect();
    cpp_files_vec.sort();
    for f in cpp_files_vec.into_iter().map(|file| format!("{out_dir}/{file}")) {
        cfg.file(f);
    }

    cfg.cpp(true)
        .flag_if_supported("-std=c++11")
        .flag_if_supported("/utf-8")
        .flag_if_supported("/bigobj")
        .warnings(false)
        .flag_if_supported("-w");

    // Enable C++ exceptions on MSVC: without /EHsc, exception handling is off, the
    // C API's try/catch blocks are inert, and a thrown exception (e.g. invalid-UTF-8
    // CSV read) aborts with STATUS_STACK_BUFFER_OVERRUN (#774). Hard flag, not
    // flag_if_supported, so a spurious probe miss can't silently drop it. gcc/clang
    // enable exceptions by default and reject /EHsc, so gate on MSVC. Mirrors
    // build_bundled_cmake.rs.
    if win_target() && is_compiler("msvc") {
        cfg.flag("/EHsc");
    }

    let is_debug = match std::env::var("DEBUG") {
        Ok(v) => v != "false" && v != "0",
        Err(_) => false,
    };
    if !is_debug {
        cfg.define("NDEBUG", None);
    }

    if win_target() {
        cfg.define("DUCKDB_BUILD_LIBRARY", None);
    }
    cfg.compile("duckdb");

    // `cc` does not link DuckDB's Windows system libs automatically (e.g. unresolved
    // `RmStartSession` from the Restart Manager). See link_windows_system_libs.
    if win_target() {
        link_windows_system_libs();
    }

    println!("cargo:lib_dir={out_dir}");
}

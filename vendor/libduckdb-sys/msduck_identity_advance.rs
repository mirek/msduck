//! Private, source-pinned DuckDB sequence advance used by IDENTITY_INSERT.
//! The archive is kept unchanged; every replacement fails on source drift.
use std::path::Path;

fn replace(root: &Path, file: &str, before: &str, after: &str) {
    let path = root.join(file);
    let source = std::fs::read_to_string(&path).expect("identity advance source");
    assert_eq!(
        source.matches(before).count(),
        1,
        "identity advance source drift: {file}"
    );
    std::fs::write(path, source.replace(before, after)).expect("patch identity advance");
}

pub fn apply(out_dir: &str) {
    println!("cargo:rerun-if-changed=msduck_identity_advance.rs");
    let root = Path::new(out_dir).join("duckdb");
    replace(
        &root,
        "src/include/duckdb/catalog/catalog_entry/sequence_catalog_entry.hpp",
        "\tint64_t NextValue(DuckTransaction &transaction);",
        "\tint64_t NextValue(DuckTransaction &transaction);\n\tbool MsduckAdvanceIdentity(DuckTransaction &transaction, int64_t candidate);",
    );
    replace(
        &root,
        "src/catalog/catalog_entry/sequence_catalog_entry.cpp",
        "void SequenceCatalogEntry::ReplayValue(uint64_t v_usage_count, int64_t v_counter) {",
        r#"// Private IDENTITY_INSERT allocator operation. The same mutex and sequence
// usage record as nextval make the high-water test and advance atomic, while
// retaining DuckDB's non-transactional allocation and WAL behavior.
bool SequenceCatalogEntry::MsduckAdvanceIdentity(DuckTransaction &transaction, int64_t candidate) {
    lock_guard<mutex> seqlock(lock);
    static constexpr const char *prefix = "__msduck_identity_";
    static constexpr auto prefix_length = sizeof("__msduck_identity_") - 1;
    const bool private_name = name.size() == prefix_length + 32 &&
        name.compare(0, prefix_length, prefix) == 0 &&
        std::all_of(name.begin() + prefix_length, name.end(), [](char ch) {
            return (ch >= '0' && ch <= '9') || (ch >= 'a' && ch <= 'f') || (ch >= 'A' && ch <= 'F');
        });
    if (!private_name || data.cycle) {
        throw SequenceException("identity advance: not a private noncycling identity sequence");
    }
    if (candidate < data.min_value || candidate > data.max_value) {
        throw SequenceException("identity advance: explicit value %lld outside sequence range", candidate);
    }
    const bool advance = data.increment > 0 ?
        (data.usage_count ? candidate > data.last_value : candidate >= data.start_value) :
        (data.usage_count ? candidate < data.last_value : candidate <= data.start_value);
    if (!advance) {
        return false;
    }
    data.last_value = candidate;
    // Match private nextval's modular terminal-counter representation.
    data.counter = MSDuckIdentityBits(uint64_t(candidate) + uint64_t(data.increment));
    data.usage_count++;
    if (!temporary) {
        transaction.PushSequenceUsage(*this, data);
    }
    return true;
}

void SequenceCatalogEntry::ReplayValue(uint64_t v_usage_count, int64_t v_counter) {"#,
    );
    replace(
        &root,
        "src/include/duckdb/function/scalar/sequence_functions.hpp",
        "struct NextvalFun {",
        r#"struct MsduckIdentityAdvanceFun {
    static constexpr const char *Name = "__msduck_identity_advance";
    static constexpr const char *Parameters = "'sequence_name', 'explicit_value'";
    static constexpr const char *Description = "Advance an msduck private identity sequence after a successful explicit write.";
    static constexpr const char *Example = "__msduck_identity_advance('main.__msduck_identity_...', 42)";
    static constexpr const char *Categories = "";

    static ScalarFunction GetFunction();
};

struct NextvalFun {"#,
    );
    replace(
        &root,
        "src/function/scalar/sequence/nextval.cpp",
        "} // namespace duckdb\n",
        r#"void MsduckIdentityAdvanceFunction(DataChunk &args, ExpressionState &state, Vector &result) {
    auto &func_expr = state.expr.Cast<BoundFunctionExpression>();
    if (!func_expr.bind_info) {
        result.SetVectorType(VectorType::CONSTANT_VECTOR);
        ConstantVector::SetNull(result, true);
        return;
    }
    auto &lstate = ExecuteFunctionState::GetFunctionState(state)->Cast<NextValLocalState>();
    UnaryExecutor::Execute<int64_t, bool>(args.data[1], result, args.size(), [&](int64_t candidate) {
        return lstate.sequence.MsduckAdvanceIdentity(lstate.transaction, candidate);
    }, FunctionErrors::CAN_THROW_RUNTIME_ERROR);
}

ScalarFunction MsduckIdentityAdvanceFun::GetFunction() {
    ScalarFunction function("__msduck_identity_advance", {LogicalType::VARCHAR, LogicalType::BIGINT},
                            LogicalType::BOOLEAN, MsduckIdentityAdvanceFunction, nullptr, nullptr);
    function.SetBindExtendedCallback(NextValBind);
    function.SetSerializeCallback(Serialize);
    function.SetDeserializeCallback(Deserialize);
    function.SetModifiedDatabasesCallback(NextValModifiedDatabases);
    function.SetInitStateCallback(NextValLocalFunction);
    function.SetVolatile();
    function.SetFallible();
    return function;
}

} // namespace duckdb
"#,
    );
    replace(
        &root,
        "src/function/function_list.cpp",
        "\tDUCKDB_SCALAR_FUNCTION(NextvalFun),",
        "\tDUCKDB_SCALAR_FUNCTION(NextvalFun),\n\tDUCKDB_SCALAR_FUNCTION(MsduckIdentityAdvanceFun),",
    );
}

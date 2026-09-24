//! Exact patches against the retained DuckDB archive; no archive mutation.
use std::path::Path;

fn replace(root: &Path, file: &str, before: &str, after: &str) {
    let path = root.join(file);
    let source = std::fs::read_to_string(&path).expect("pending drain source");
    assert_eq!(
        source.matches(before).count(),
        1,
        "pending drain source drift: {file}"
    );
    std::fs::write(path, source.replace(before, after)).expect("patch pending drain");
}

pub fn apply(out_dir: &str) {
    println!("cargo:rerun-if-changed=msduck_pending_drain.rs");
    let root = Path::new(out_dir).join("duckdb");
    replace(
        &root,
        "src/include/duckdb/main/pending_query_result.hpp",
        "\tDUCKDB_API void Close();",
        "\tDUCKDB_API void Close();\n\tDUCKDB_API int MsduckCancelReadAndDrain();",
    );
    replace(
        &root,
        "src/main/pending_query_result.cpp",
        "#include \"duckdb/main/prepared_statement_data.hpp\"",
        "#include \"duckdb/main/prepared_statement_data.hpp\"\n#include \"duckdb/main/database.hpp\"\n#include \"duckdb/main/valid_checker.hpp\"",
    );
    replace(
        &root,
        "src/main/pending_query_result.cpp",
        "void PendingQueryResult::Close() {",
        r#"// msduck private ABI: 0 cancelled/drained, 1 native or cleanup error,
// 2 unsupported statement (unchanged). The caller owns all request operations.
int PendingQueryResult::MsduckCancelReadAndDrain() {
    if (HasError()) {
        return 1;
    }
    auto lock = LockContext();
    CheckExecutableInternal(*lock); // stale handles must not interrupt a newer query
    if (statement_type != StatementType::SELECT_STATEMENT || !properties.IsReadOnly()) {
        return 2;
    }
    auto retained_context = context;
    auto &executor = context->GetExecutor();
    context->Interrupt();
    executor.CancelTasks(); // join background work before reading its final error
    auto error = executor.HasError() ? executor.GetError() : ErrorData(InterruptException());
    bool native_error = error.Type() != ExceptionType::INTERRUPT;
    bool invalidate = native_error && context->ErrorInvalidatesTransaction(error.Type());
    if (native_error && (Exception::InvalidatesDatabase(error.Type()) || error.Type() == ExceptionType::INTERNAL)) {
        ValidChecker::Invalidate(DatabaseInstance::GetDatabase(*context), error.RawMessage());
    }
    context->ProcessError(error, context->GetCurrentQuery());
    SetError(error); // own the error before EndQueryInternal destroys the executor
    auto cleanup_error = context->EndQueryInternal(*lock, false, invalidate, error);
    if (cleanup_error.HasError()) {
        SetError(ErrorData(cleanup_error.Type(), cleanup_error.RawMessage() +
            "\nPrior execution error: " + error.RawMessage()));
        native_error = true;
    }
    context->ClearInterrupt(); // workers are joined; no delayed interrupt is permitted
    Close();
    return native_error ? 1 : 0;
}

void PendingQueryResult::Close() {"#,
    );
    replace(
        &root,
        "src/main/capi/pending-c.cpp",
        "void duckdb_destroy_pending(duckdb_pending_result *pending_result) {",
        r#"// Private msduck API: retained error is readable through duckdb_pending_error.
extern "C" DUCKDB_API int msduck_pending_cancel_read_and_drain(duckdb_pending_result pending_result) {
    if (!pending_result) {
        return 1;
    }
    auto wrapper = reinterpret_cast<PendingStatementWrapper *>(pending_result);
    if (!wrapper->statement) {
        return 1;
    }
    try {
        return wrapper->statement->MsduckCancelReadAndDrain();
    } catch (std::exception &ex) {
        wrapper->statement->SetError(duckdb::ErrorData(ex));
    } catch (...) {
        wrapper->statement->SetError(duckdb::ErrorData("Unhandled exception draining pending read"));
    }
    return 1;
}

void duckdb_destroy_pending(duckdb_pending_result *pending_result) {"#,
    );
}

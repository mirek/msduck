use duckdb::{
    core::{DataChunkHandle, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck::{
    engine::Session,
    read_cancellation::{Mode, Outcome},
    server::Server,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
struct Marker;
impl VScalar for Marker {
    type State = Arc<AtomicBool>;
    fn volatile() -> bool {
        true
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Integer.into()],
            LogicalTypeId::Integer.into(),
        )]
    }
    fn invoke(
        state: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        unsafe {
            output
                .flat_vector()
                .as_mut_slice_with_len::<i32>(input.len())
                .fill(1)
        };
        state.store(true, Ordering::Release);
        Ok(())
    }
}
fn hex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn expected(tokens: &serde_json::Value, descriptor: u64) -> Vec<u8> {
    let mut out = vec![];
    for t in tokens.as_array().unwrap() {
        match t["token"].as_u64().unwrap() {
            id @ (253..=255) => msduck_tds::done(
                &mut out,
                id as u8,
                t["status"].as_u64().unwrap() as u16,
                t["command"].as_u64().unwrap() as u16,
                t["count"].as_str().unwrap().parse().unwrap(),
            ),
            129 => out.extend(hex(t["rawHex"].as_str().unwrap())),
            227 => msduck_tds::transaction_env(&mut out, 10, descriptor),
            other => panic!("unexpected fixture token {other}"),
        }
    }
    out
}
#[test]
fn cancelled_computations_match_reference_completion_and_preserve_session_work() {
    const CHILD: &str = "MSDUCK_ENGINE_READ_CANCEL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cancelled_computations_match_reference_completion_and_preserve_session_work",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("engine read cancellation probe exceeded 90 seconds")
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../reference/attention-compute.json")).unwrap();
    for case in fixture["results"].as_array().unwrap() {
        let e = &case["entry"];
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        session
            .db
            .register_scalar_function_with_state::<Marker>("msduck_cancel_marker", &flag)
            .unwrap();
        let explicit = e["transaction"].as_bool().unwrap();
        let abort = e["xactAbort"].as_bool().unwrap();
        let in_try = e["tryCatch"].as_bool().unwrap();
        let setup = format!(
            "CREATE TABLE dbo.cancel_probe(n INT); SET XACT_ABORT {}; SET DATEFIRST 2; {}",
            if abort { "ON" } else { "OFF" },
            if explicit { "BEGIN TRANSACTION" } else { "" }
        );
        let (setup_tokens, ok) = session.batch_response(&setup, &Default::default(), false, None);
        assert!(ok, "{setup_tokens:?}");
        let descriptor = session.transaction_descriptor;
        let mode = match e["mode"].as_str().unwrap() {
            "batch" => Mode::Batch,
            "rpc" => Mode::Rpc,
            "prepared" => Mode::Prepared,
            _ => unreachable!(),
        };
        let body = "INSERT dbo.cancel_probe VALUES(1); IF @hold=1 SELECT SUM(CAST(msduck_cancel_marker(a.object_id) AS FLOAT)*b.object_id) AS work FROM sys.all_objects a CROSS JOIN sys.all_objects b CROSS JOIN sys.all_objects c; INSERT dbo.cancel_probe VALUES(2); SELECT 42 AS completed";
        let sql = if in_try {
            format!(
                "BEGIN TRY {body} END TRY BEGIN CATCH INSERT dbo.cancel_probe VALUES(3); SELECT ERROR_NUMBER() AS caught END CATCH"
            )
        } else {
            body.to_string()
        };
        let mut parameters = HashMap::new();
        parameters.insert(
            "@hold".into(),
            msduck::parameter::Parameter {
                value: msduck_core::value::Value::Int(1),
                data_type: msduck_core::types::Type::Int,
            },
        );
        let outcome =
            session.batch_response_with_read_cancel(&sql, &parameters, mode, flag.clone());
        let Outcome::Cancelled {
            tokens,
            attention_ack,
        } = outcome
        else {
            panic!("not cancelled for {e}: {outcome:?}")
        };
        assert!(flag.load(Ordering::Acquire));
        assert_eq!(tokens, expected(&case["responses"][0], descriptor), "{e}");
        assert_eq!(
            attention_ack.as_slice(),
            expected(&case["responses"][1], descriptor),
            "{e}"
        );
        assert_eq!(session.transactions, u32::from(explicit && !abort));
        let rows = session
            .db
            .prepare("SELECT n FROM dbo.cancel_probe ORDER BY n")
            .unwrap()
            .query_map([], |r| r.get::<_, i32>(0))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            if explicit && abort { vec![] } else { vec![1] },
            "{e}"
        );
        let (_, ok) = session.batch_response(
            "IF @@DATEFIRST<>2 THROW 51000,'DATEFIRST changed',1; SELECT 1 AS reusable",
            &Default::default(),
            false,
            None,
        );
        assert!(ok, "{e}");
        if session.transactions > 0 {
            session.rollback_transaction("").unwrap();
        }
        eprintln!("matched engine read cancellation {e}");
    }
}

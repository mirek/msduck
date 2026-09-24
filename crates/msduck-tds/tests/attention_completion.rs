use msduck_tds::attention_completion::{ACK, ActiveRead};
fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn active_read_completion_matches_both_raw_runs_of_all_24_cases() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../reference/attention-compute.json")).unwrap();
    for run in fixture["rawCaptures"].as_array().unwrap() {
        assert_eq!(run.as_array().unwrap().len(), 24);
        for (index, capture) in run.as_array().unwrap().iter().enumerate() {
            let entry = &capture["entry"];
            let plan = ActiveRead::new(
                entry["mode"] != "batch",
                entry["transaction"].as_bool().unwrap(),
                entry["xactAbort"].as_bool().unwrap(),
                entry["tryCatch"].as_bool().unwrap(),
            );
            let metadata = fixture["results"][index]["responses"][0]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["token"] == 129)
                .unwrap();
            let metadata = bytes(metadata["rawHex"].as_str().unwrap());
            let response = bytes(capture["responses"][0].as_str().unwrap());
            let at = response
                .windows(metadata.len())
                .position(|b| b == metadata)
                .unwrap()
                + metadata.len();
            let mut expected = vec![];
            plan.before_rollback(&mut expected);
            if plan.rollback {
                let descriptor = bytes(capture["transactionDescriptor"].as_str().unwrap());
                msduck_tds::transaction_env(
                    &mut expected,
                    10,
                    u64::from_le_bytes(descriptor.try_into().unwrap()),
                );
            }
            plan.finish(&mut expected);
            assert_eq!(expected, response[at..], "{entry}");
            assert_eq!(
                ACK.as_slice(),
                bytes(capture["responses"][1].as_str().unwrap()),
                "{entry}"
            );
        }
    }
}

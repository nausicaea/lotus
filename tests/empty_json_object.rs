use lotus::default_runner;
use serde_json::{json, to_writer};
use std::fs::{create_dir, File};
use std::io::Write;

#[tokio::test]
async fn successful_run() -> anyhow::Result<()> {
    let input_data = json! {{}};
    let expected_data = json! {{
        "http": {
            "method": "POST",
            "request": {
                "mime_type": "application/json",
                "body": {
                    "bytes": "2",
                },
            },
            "version": "HTTP/1.1",
        },
        "dummy": "true",
        "url": {
            "port": 5066,
            "path": "/",
            "domain": "127.0.0.1",
        },
        "@timestamp": "2023-07-16T13:35:40.682842093Z",
        "@version": "1",
        "event": {
            "original": "{}",
        },
        "host": {
            "ip": "172.17.0.1",
        },
    }};

    let rules_dir = "rules";
    let rule_file = "00-dummy.conf";
    let rule_data = r#"filter { mutate { add_field => { "[dummy]" => "true" } } }"#;

    let tests_dir = "tests";
    let input_file = "input.json";
    let expected_file = "expected.json";

    let tmp_dir = tempfile::tempdir()?;

    // Write the rules
    let rules_dir = tmp_dir.path().join(&rules_dir);
    create_dir(&rules_dir)?;
    File::create(&rules_dir.join(&rule_file))?.write_all(rule_data.as_bytes())?;

    // Write the tests
    let tests_dir = tmp_dir.path().join(&tests_dir);
    create_dir(&tests_dir)?;

    for t in &["a", "b", "c", "d"] {
        let test_dir = tests_dir.join(t);
        create_dir(&test_dir)?;
        let input = File::create(&test_dir.join(&input_file))?;
        to_writer(input, &input_data)?;
        let expected = File::create(&test_dir.join(&expected_file))?;
        to_writer(expected, &expected_data)?;
    }

    // Call the test runner
    default_runner(tmp_dir.path()).await?;

    Ok(())
}

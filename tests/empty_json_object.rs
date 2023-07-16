use lotus::default_runner;
use std::fs::{create_dir, File};
use std::io::Write;

#[tokio::test]
async fn successful_run() {
    let input_data = "{}";
    let expected_data = "{}";
    let rule_data = r#"filter { mutate { add_field => { "[dummy]" => "true" } } }"#;

    let tmp_dir = tempfile::tempdir().unwrap();

    // Write the rules
    let rules_dir = tmp_dir.path().join("rules");
    create_dir(&rules_dir).unwrap();
    File::create(&rules_dir.join("00-dummy.conf"))
        .unwrap()
        .write_all(rule_data.as_bytes())
        .unwrap();

    // Write the test
    let tests_dir = tmp_dir.path().join("tests");
    create_dir(&tests_dir).unwrap();
    let test_dir = tests_dir.join("a");
    create_dir(&test_dir).unwrap();
    File::create(&test_dir.join("input.json"))
        .unwrap()
        .write_all(input_data.as_bytes())
        .unwrap();
    File::create(&test_dir.join("expected.json"))
        .unwrap()
        .write_all(expected_data.as_bytes())
        .unwrap();

    default_runner(tmp_dir.path()).await.unwrap();
}

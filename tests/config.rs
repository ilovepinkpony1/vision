mod common;

use common::Home;
use vision::config::Config;

#[test]
fn config_prints_the_configuration_in_use_as_toml() {
    let home = Home::init();
    let run = home.run(&["config"]);
    run.expect_code(0);
    assert!(
        run.stdout.starts_with("# The configuration in use: "),
        "{run:?}"
    );
    let printed = Config::parse(&run.stdout).expect("the output is a valid configuration");
    assert_eq!(printed, Config::parse(&home.read("config.toml")).unwrap());
}

#[test]
fn config_json_prints_the_same_values() {
    let home = Home::init();
    let run = home.run(&["config", "--json"]);
    run.expect_code(0);
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["project"], "demo");
    assert_eq!(json["budget"]["worker"], 25.0);
    assert_eq!(json["limits"]["seven_day"], 0.85);
    assert_eq!(json["judge"]["always_ask"][0], "security");
}

#[test]
fn a_missing_key_takes_its_default() {
    let home = Home::init();
    std::fs::write(
        home.path("config.toml"),
        "project = \"other\"\n[budget]\nworker = 40\n",
    )
    .unwrap();
    let run = home.run(&["config", "--json"]);
    run.expect_code(0);
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["project"], "other");
    assert_eq!(json["budget"]["worker"], 40.0);
    assert_eq!(json["budget"]["reader"], 3.0);
    assert_eq!(
        json["claude"]["config_dir"],
        home.path("claude").display().to_string()
    );
}

#[test]
fn an_unknown_key_is_an_error_that_names_the_key() {
    let home = Home::init();
    let text = home
        .read("config.toml")
        .replace("week = 300", "week = 300\nwekk = 1");
    std::fs::write(home.path("config.toml"), text).unwrap();
    let run = home.run(&["config"]);
    run.expect_code(1);
    assert!(run.stderr.contains("wekk"), "{run:?}");
    assert!(run.stderr.contains("config.toml is not valid"), "{run:?}");
    // The other commands that read the configuration refuse it too, but the prompt never fails.
    home.run(&["status"]).expect_code(1);
    let prompt = home.run(&["prompt"]);
    prompt.expect_code(0);
    assert!(prompt.stdout.starts_with("V:"), "{prompt:?}");
}

#[test]
fn config_before_init_says_what_to_do() {
    let home = Home::new();
    let run = home.run(&["config"]);
    run.expect_code(1);
    assert!(run.stderr.contains("vision init"), "{run:?}");
}

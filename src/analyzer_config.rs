use anyhow::Context as _;
use anyhow::Result;
use serde_json::Value;
use serde_json::Map;

pub(crate) fn without_test_cfg(mut configuration: Value) -> Result<Value> {
    let settings = configuration.as_object_mut().context("rust-analyzer config must be a JSON object")?;
    settings
        .entry("cfg")
        .or_insert_with(|| Value::Object(Map::default()))
        .as_object_mut()
        .context("rust-analyzer cfg settings must be a JSON object")?
        .insert("setTest".into(), Value::Bool(false));
    if let Some(explicit_cfgs) = configuration.pointer_mut("/cargo/cfgs") {
        /* An explicit `test` in cargo.cfgs re-enables cfg(test);
        leaving an absent list untouched preserves rust-analyzer defaults. */
        explicit_cfgs
            .as_array_mut()
            .context("rust-analyzer cargo.cfgs must be a JSON array")?
            .retain(|option| option.as_str() != Some("test"));
    }
    Ok(configuration)
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use serde_json::Value;
    use serde_json::json;

    use super::without_test_cfg;

    #[rstest]
    #[case(json!({}), json!({"cfg": {"setTest": false}}))]
    #[case(json!({"cfg": {"setTest": true, "other": 7}, "cargo": {"features": ["http"], "target": "custom"}}), json!({"cfg": {"setTest": false, "other": 7}, "cargo": {"features": ["http"], "target": "custom"}}))]
    #[case(json!({"cargo": {"cfgs": ["test", "debug_assertions", "feature=\"test\"", "!test"]}}), json!({"cfg": {"setTest": false}, "cargo": {"cfgs": ["debug_assertions", "feature=\"test\"", "!test"]}}))]
    #[case(json!({"cfg": {"setTest": false}, "cargo": {"cfgs": []}}), json!({"cfg": {"setTest": false}, "cargo": {"cfgs": []}}))]
    fn test_production_configuration(#[case] configuration: Value, #[case] expected: Value) -> Result<()> {
        let system_under_test = without_test_cfg(configuration)?;

        assert_eq!(system_under_test, expected, "Production pass changed unrelated settings");
        Ok(())
    }

    #[rstest]
    #[case(json!(null))]
    #[case(json!([]))]
    #[case(json!({"cfg": null}))]
    #[case(json!({"cargo": {"cfgs": "test"}}))]
    fn test_invalid_configuration(#[case] configuration: Value) {
        let system_under_test = without_test_cfg(configuration);

        assert!(system_under_test.is_err(), "Invalid configuration was accepted");
    }
}

//! The monitor subprocess must not inherit real configuration or store overrides.
mod harness;
use std::collections::BTreeMap;
use std::process::Command;

#[test]
fn monitor_fixture_overrides_inherited_configuration_and_store_paths() {
    let _serial = harness::serial();
    let root = harness::base("fixture-environment");
    let stores = harness::Stores::new(&root);
    for fixture in [Some(&stores), None] {
        let mut cmd = Command::new("synthetic-monitor");
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "AGENT_MONITOR_STATE",
            "CLAUDE_MONITOR_STATE",
            "AGENT_MONITOR_CACHE",
            "CLAUDE_MONITOR_CACHE",
            "CODEX_HOME",
            "CODEX_SESSIONS_DIR",
            "QODERWORK_DB",
            "QODER_TASKS_ROOT",
        ] {
            cmd.env(key, "/synthetic/inherited");
        }
        harness::configure_monitor_environment(&mut cmd, &root, 3195, fixture);
        let values: BTreeMap<_, _> = cmd
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|v| {
                    (
                        key.to_string_lossy().into_owned(),
                        std::path::PathBuf::from(v),
                    )
                })
            })
            .collect();
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "AGENT_MONITOR_STATE",
            "CLAUDE_MONITOR_STATE",
            "AGENT_MONITOR_CACHE",
            "CLAUDE_MONITOR_CACHE",
            "CODEX_HOME",
            "CODEX_SESSIONS_DIR",
            "QODERWORK_DB",
            "QODER_TASKS_ROOT",
        ] {
            assert!(
                values.get(key).is_some_and(|p| p.starts_with(&root)),
                "{key} escaped its fixture: {values:?}"
            );
        }
        assert_eq!(values["AGENT_MONITOR_STATE"], root.join("state-3195"));
        assert_eq!(
            values["AGENT_MONITOR_STATE"],
            values["CLAUDE_MONITOR_STATE"]
        );
        assert_eq!(
            values["AGENT_MONITOR_CACHE"],
            values["CLAUDE_MONITOR_CACHE"]
        );
        assert_eq!(
            values["CODEX_SESSIONS_DIR"],
            stores.root.join("codex/sessions")
        );
    }
}

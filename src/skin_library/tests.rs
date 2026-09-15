use super::*;
use crate::scene3d::skin::tests::{png, sample_image};

#[test]
fn managed_imports_survive_source_removal_deduplicate_and_reject_invalid_pngs() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("我的皮肤.png");
    let root = directory.path().join("skins");
    let bytes = png(&sample_image(BodyType::Alex, false));
    fs::write(&source, &bytes).unwrap();
    let first = import_file(&root, &source).unwrap();
    let duplicate = import_file(&root, &source).unwrap();
    assert_eq!(first.entry, duplicate.entry);
    assert_eq!(first.entry.name, "我的皮肤");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    fs::remove_file(&source).unwrap();
    let managed = managed_path(&root, &first.entry.id).unwrap();
    assert_eq!(
        Skin::decode(&read_png(&managed).unwrap()).unwrap().model,
        BodyType::Alex
    );
    fs::write(&source, b"broken PNG").unwrap();
    assert!(import_file(&root, &source).is_err());
    assert_eq!(fs::read(&managed).unwrap(), bytes);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    assert!(managed_path(&root, "../../settings.json").is_err());
}

#[test]
fn saved_skin_selection_and_model_override_round_trip_through_preferences() {
    use crate::preferences::{AppPreferences, PreferencesStore};
    let directory = tempfile::tempdir().unwrap();
    let (mut store, mut prefs, _) =
        PreferencesStore::load(directory.path(), AppPreferences::default());
    prefs.skins = Preferences {
        selected: Some("a".repeat(64)),
        entries: vec![Entry {
            id: "a".repeat(64),
            name: "Skin".into(),
            model: Some(BodyType::Alex),
        }],
    };
    store.save(&prefs).unwrap();
    let (_, restored, warning) =
        PreferencesStore::load(directory.path(), AppPreferences::default());
    assert!(warning.is_none());
    assert_eq!(restored.skins, prefs.skins);
    let old: AppPreferences = serde_json::from_str("{}").unwrap();
    assert_eq!(old.skins, Preferences::default());
}

#[test]
#[ignore = "set MACINDECODE_STEVE_SKIN and MACINDECODE_ALEX_SKIN to local reference PNGs"]
fn imports_reference_skins_with_the_expected_body_types() {
    let directory = tempfile::tempdir().unwrap();
    for (variable, model) in [
        ("MACINDECODE_STEVE_SKIN", BodyType::Steve),
        ("MACINDECODE_ALEX_SKIN", BodyType::Alex),
    ] {
        let source = PathBuf::from(std::env::var_os(variable).expect(variable));
        let loaded = import_file(directory.path(), &source).unwrap();
        assert_eq!(loaded.skin.detected, model, "{}", source.display());
        assert_eq!(loaded.skin.model, model);
    }
}

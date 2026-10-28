//! ER layout must match GokuCore exactly: every case in Tests/Parity/er-layout (written by ParityFixtureTests.swift).
use goku_engine::er::{Edge, Options, Rect, Size, layout};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

#[derive(Deserialize)]
struct Input {
    entities: Vec<String>,
    sizes: HashMap<String, Size>,
    edges: Vec<Edge>,
    options: Options,
}

#[derive(Deserialize)]
struct Fixture {
    input: Input,
    output: BTreeMap<String, Rect>,
}

fn parity_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../Tests/Parity/er-layout")
}

#[test]
fn every_er_layout_fixture_matches_swift() {
    let mut cases: Vec<_> = std::fs::read_dir(parity_dir())
        .expect("Tests/Parity/er-layout is missing")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 6, "expected the fixture cases, found {}", cases.len());
    for path in cases {
        let f: Fixture = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let got = layout(&f.input.entities, &f.input.sizes, &f.input.edges, &f.input.options);
        let name = path.file_name().unwrap().to_string_lossy();
        assert_eq!(got.keys().collect::<Vec<_>>(), f.output.keys().collect::<Vec<_>>(), "{name}: entities");
        for (id, want) in &f.output {
            assert_eq!(&got[id], want, "{name}: {id}");
        }
    }
}

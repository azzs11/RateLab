//! Every prose template renders against the committed result files: a fact that was
//! renamed or removed fails here, not in a published document.

use bench::report::Inputs;
use bench::results::read_json;
use std::path::Path;

fn committed() -> Inputs {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../results");
    Inputs {
        local: read_json(&dir.join("local.json")).unwrap(),
        memory: read_json(&dir.join("memory.json")).unwrap(),
        cluster: read_json(&dir.join("cluster.json")).unwrap(),
        strategies: read_json(&dir.join("strategies.json")).unwrap(),
        traces: read_json(&dir.join("traces.json")).unwrap(),
        real_strategies: read_json(&dir.join("real_strategies.json")).unwrap(),
        overload: read_json(&dir.join("overload.json")).unwrap(),
        llm: read_json(&dir.join("llm.json")).unwrap(),
    }
}

#[test]
fn every_template_renders_with_the_committed_results() {
    let facts = bench::story::facts(&committed());
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let mut rendered = 0;
    for dir in ["report", "story"] {
        let Ok(entries) = std::fs::read_dir(docs.join(dir)) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.to_string_lossy().ends_with(".tmpl.md") {
                let text = std::fs::read_to_string(&path).unwrap();
                let out = bench::story::render(&text, &facts)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                assert!(!out.contains("{{"), "{}", path.display());
                rendered += 1;
            }
        }
    }
    assert!(rendered >= 1, "no templates found");
}

#[test]
fn the_generated_report_is_up_to_date() {
    let facts = bench::story::facts(&committed());
    let report = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/report");
    let template = std::fs::read_to_string(report.join("the-price-of-exactness.tmpl.md")).unwrap();
    let expected = bench::story::render(&template, &facts).unwrap();
    let actual = std::fs::read_to_string(report.join("the-price-of-exactness.md")).unwrap();
    assert!(expected == actual, "docs/report is stale: run `just story`");
}

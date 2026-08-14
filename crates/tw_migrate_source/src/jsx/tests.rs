use super::{ProofOutcome, prepare, prove_prepared};
use tw_migrate_css::{Relation, SelectorKey};

const CSS: &str = "src/App.module.css";

fn class(name: &str) -> SelectorKey {
    SelectorKey::Class(name.to_string())
}

fn run(files: &[(&str, &str)], relation: Relation, ancestor: &str, target: &str) -> ProofOutcome {
    prove_prepared(
        &prepare(files, CSS),
        &class(ancestor),
        relation,
        &class(target),
    )
}

#[test]
fn direct_nesting_proves_child_and_descendant() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
export function App() {
  return <div className={styles.parent}><span className={styles.child} /></div>;
}
"#,
    )];
    for relation in [Relation::Child, Relation::Descendant] {
        let outcome = run(&files, relation, "parent", "child");
        assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
        assert_eq!(outcome.usages.len(), 1);
        assert!(outcome.usages[0].proven);
        assert_eq!(outcome.reason, None);
    }
}

#[test]
fn deep_nesting_proves_descendant_only() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
export function App() {
  return <div className={styles.parent}><section><span className={styles.child} /></section></div>;
}
"#,
    )];
    let descendant = run(&files, Relation::Descendant, "parent", "child");
    assert!(descendant.aggregate_proven, "{descendant:?}");
    let child = run(&files, Relation::Child, "parent", "child");
    assert!(!child.aggregate_proven);
    assert_eq!(child.reason, Some("unproven-ancestry"));
}

#[test]
fn local_component_proves_via_render_sites() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
function Inner() {
  return <span className={styles.child} />;
}
export function App() {
  return <div className={styles.parent}><Inner /></div>;
}
"#,
    )];
    for relation in [Relation::Child, Relation::Descendant] {
        let outcome = run(&files, relation, "parent", "child");
        assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
        assert_eq!(outcome.usages.len(), 1);
    }
}

#[test]
fn extensionless_import_resolves() {
    let files = [
        (
            "src/App.tsx",
            r#"import styles from "./App.module.css";
import Title from "./Title";
export function App() {
  return <div className={styles.parent}><Title /></div>;
}
"#,
        ),
        (
            "src/Title.tsx",
            r#"import styles from "./App.module.css";
export default function Title() {
  return <h1 className={styles.child} />;
}
"#,
        ),
    ];
    for relation in [Relation::Child, Relation::Descendant] {
        let outcome = run(&files, relation, "parent", "child");
        assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
    }
}

#[test]
fn unresolved_import_disqualifies() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
import Missing from "./Missing";
export function App() {
  return <div className={styles.parent}><Missing className={styles.child} /></div>;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven);
    assert_eq!(outcome.usages.len(), 1);
    assert_eq!(
        outcome.usages[0].reason,
        Some("unresolved-component-import")
    );
}

#[test]
fn map_callback_counts_as_static() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
export function App({ items }) {
  return <ul className={styles.parent}>{items.map((item) => <li className={styles.child} key={item} />)}</ul>;
}
"#,
    )];
    for relation in [Relation::Child, Relation::Descendant] {
        let outcome = run(&files, relation, "parent", "child");
        assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
    }
}

#[test]
fn conditional_map_callback_disqualifies() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
export function App({ items }) {
  return <ul className={styles.parent}>{items.map((item) => item.on ? <li className={styles.child} /> : null)}</ul>;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven);
    assert_eq!(outcome.reason, Some("dynamic-content-boundary"));
}

#[test]
fn conditional_return_disqualifies() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
function Inner(props) {
  if (props.compact) {
    return <span className={styles.child} />;
  }
  return <span className={styles.child} />;
}
export function App() {
  return <div className={styles.parent}><Inner /></div>;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven);
    assert_eq!(outcome.usages.len(), 2);
    for usage in &outcome.usages {
        assert_eq!(usage.reason, Some("conditional-return"));
    }
    assert_eq!(outcome.reason, Some("conditional-return"));
}

#[test]
fn mixed_direct_usages_report_per_usage() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
export function App() {
  return (
    <div>
      <div className={styles.parent}><span className={styles.child} /></div>
      <section><span className={styles.child} /></section>
    </div>
  );
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven);
    assert_eq!(outcome.usages.len(), 2);
    assert!(outcome.usages[0].proven);
    assert!(!outcome.usages[1].proven);
    assert_eq!(outcome.usages[1].reason, Some("unproven-ancestry"));
}

#[test]
fn all_render_sites_must_match() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
function Inner() {
  return <span className={styles.child} />;
}
export function App() {
  return (
    <div>
      <div className={styles.parent}><Inner /></div>
      <section><Inner /></section>
    </div>
  );
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven);
    assert_eq!(outcome.usages.len(), 1);
    assert_eq!(outcome.usages[0].reason, Some("unproven-ancestry"));
}

#[test]
fn class_name_forwarding_proves() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
function Btn(props) {
  return <button className={props.className} />;
}
export function App() {
  return <div className={styles.parent}><Btn className={styles.child} /></div>;
}
"#,
    )];
    for relation in [Relation::Child, Relation::Descendant] {
        let outcome = run(&files, relation, "parent", "child");
        assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
        assert_eq!(outcome.usages.len(), 1);
    }
}

#[test]
fn conditional_forwarding_disqualifies() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
function Btn(props) {
  return <button className={props.solid ? props.className : ""} />;
}
export function App() {
  return <div className={styles.parent}><Btn className={styles.child} /></div>;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven);
    assert_eq!(outcome.usages.len(), 1);
    assert_eq!(outcome.usages[0].reason, Some("dynamic-content-boundary"));
}

#[test]
fn unused_class_reports_no_usages() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
export function App() {
  return <div className={styles.parent} />;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven);
    assert!(outcome.usages.is_empty());
    assert_eq!(outcome.reason, Some("no-usages"));
}

#[test]
fn self_recursive_component_terminates() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
function Tree() {
  return <div className={styles.node}><span className={styles.child} /><Tree /></div>;
}
export function App() {
  return <div className={styles.parent}><Tree /></div>;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven);
    assert_eq!(outcome.usages.len(), 1);
    assert_eq!(outcome.usages[0].reason, Some("recursive-component"));
}

#[test]
fn portal_disqualifies() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
import { createPortal } from "react-dom";
function Modal(props) {
  return createPortal(<div className={styles.child} />, props.host);
}
export function App() {
  return <div className={styles.parent}><Modal /></div>;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven);
    assert_eq!(outcome.usages.len(), 1);
    assert_eq!(outcome.usages[0].reason, Some("portal"));
}

#[test]
fn children_passthrough_proves() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
function Wrapper(props) {
  return <div className={styles.parent}>{props.children}</div>;
}
export function App() {
  return <main><Wrapper><span className={styles.child} /></Wrapper></main>;
}
"#,
    )];
    for relation in [Relation::Child, Relation::Descendant] {
        let outcome = run(&files, relation, "parent", "child");
        assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
    }
}

#[test]
fn deep_children_passthrough_denies_child() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
function Wrapper(props) {
  return <div className={styles.parent}><section>{props.children}</section></div>;
}
export function App() {
  return <main><Wrapper><span className={styles.child} /></Wrapper></main>;
}
"#,
    )];
    let descendant = run(&files, Relation::Descendant, "parent", "child");
    assert!(descendant.aggregate_proven, "{descendant:?}");
    let child = run(&files, Relation::Child, "parent", "child");
    assert!(!child.aggregate_proven);
    assert_eq!(child.reason, Some("unproven-ancestry"));
}

#[test]
fn export_class_component_usage_disqualifies() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
export class Legacy {
  render() {
    return <span className={styles.child} />;
  }
}
export function App() {
  return <div className={styles.parent}><span className={styles.child} /></div>;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven, "{outcome:?}");
    assert_eq!(outcome.usages.len(), 2);
    assert_eq!(outcome.reason, Some("dynamic-content-boundary"));
}

#[test]
fn spread_props_on_ancestor_disqualify() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
export function App({ extra }) {
  return <div className={styles.parent} {...extra}><span className={styles.child} /></div>;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven, "{outcome:?}");
    assert_eq!(outcome.usages.len(), 1);
    assert_eq!(outcome.usages[0].reason, Some("dynamic-content-boundary"));
}

#[test]
fn spread_props_on_unrelated_sibling_still_prove() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
export function App({ extra }) {
  return <div className={styles.parent}><em {...extra} /><span className={styles.child} /></div>;
}
"#,
    )];
    for relation in [Relation::Child, Relation::Descendant] {
        let outcome = run(&files, relation, "parent", "child");
        assert!(outcome.aggregate_proven, "{relation:?}: {outcome:?}");
    }
}

#[test]
fn hoc_wrapped_component_disqualifies() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
import { withTheme } from "./theme";
function Inner() {
  return <span className={styles.child} />;
}
const Fancy = withTheme(Inner);
export function App() {
  return <div className={styles.parent}><Fancy /></div>;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven, "{outcome:?}");
    assert_eq!(outcome.usages.len(), 1);
    assert_eq!(outcome.usages[0].reason, Some("hoc-or-dynamic-component"));
}

#[test]
fn dynamic_component_tag_disqualifies() {
    let files = [(
        "src/App.tsx",
        r#"import styles from "./App.module.css";
function Section() {
  return <span className={styles.child} />;
}
const Tag = flag ? Section : "div";
export function App() {
  return <div className={styles.parent}><Tag /></div>;
}
"#,
    )];
    let outcome = run(&files, Relation::Descendant, "parent", "child");
    assert!(!outcome.aggregate_proven, "{outcome:?}");
    assert_eq!(outcome.usages.len(), 1);
    assert_eq!(outcome.usages[0].reason, Some("hoc-or-dynamic-component"));
}
